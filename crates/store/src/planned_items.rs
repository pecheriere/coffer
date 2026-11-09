//! Planned items (spec §6.7): what the household expects to spend or receive.
//! Every value read is parsed back into its domain type, so a row that no
//! longer fits is a backend error.

use std::collections::HashMap;

use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::Money;
use coffer_domain::plan::{
    CalendarMonth, IntervalUnit, PlanDirection, PlanItemType, PlanSchedule, PlanSpending, PlanTier,
    PlanValues, PlanVersion, PlanVersions, PlannedItemName,
};
use coffer_domain::transactions::Notes;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

mod versions;

macro_rules! columns {
    () => {
        "id, name, account_hint_id, notes, active, item_type, every, unit, anchor_on, end_on, \
         one_off_on, goal_start_on, goal_due_on, goal_completed, created_at, updated_at"
    };
}

/// A stored planned item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItemRecord {
    pub id: PlannedItemId,
    pub name: PlannedItemName,
    /// The category, amount, direction, spending kind and tier below are the version in force
    /// in the month the read named; [`Self::versions`] has them all.
    pub category: CategoryId,
    /// A positive magnitude; [`Self::direction`] says which way it goes.
    pub amount: Money,
    pub direction: PlanDirection,
    pub account_hint: Option<AccountId>,
    pub notes: Option<Notes>,
    pub active: bool,
    pub schedule: PlanSchedule,
    /// Set on a recurring outflow, and only there.
    pub spending: Option<PlanSpending>,
    /// Set on every outflow, and only there: saving toward on a goal, and on nothing else.
    pub tier: Option<PlanTier>,
    /// Every version of the values above, oldest write first.
    pub versions: PlanVersions,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl PlannedItemRecord {
    /// The values in force in `month`.
    #[must_use]
    pub fn values_in(&self, month: CalendarMonth) -> PlanValues {
        *self.versions.in_force(month)
    }
}

/// A planned item to create.
#[derive(Debug, Clone)]
pub struct NewPlannedItem {
    pub name: PlannedItemName,
    pub category: CategoryId,
    pub amount: Money,
    pub direction: PlanDirection,
    pub account_hint: Option<AccountId>,
    pub notes: Option<Notes>,
    pub active: bool,
    pub schedule: PlanSchedule,
    /// Set on a recurring outflow, and only there: the table refuses others.
    pub spending: Option<PlanSpending>,
    /// Set on every outflow: the table refuses one on income, and saving toward on anything
    /// but a goal or anything else on a goal.
    pub tier: Option<PlanTier>,
    /// The household's day, which the first version is written on.
    pub written_on: Date,
    pub created_at: Timestamp,
}

/// The fields of a planned item to change; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlannedItemChanges {
    pub name: Option<PlannedItemName>,
    /// `Some(None)` clears the hint.
    pub account_hint: Option<Option<AccountId>>,
    /// `Some(None)` clears the notes.
    pub notes: Option<Option<Notes>>,
    pub active: Option<bool>,
    /// A schedule of the item's own type: the table refuses any other.
    pub schedule: Option<PlanSchedule>,
}

/// The schedule columns of one row, each `None` where its type has none.
#[derive(Default)]
struct ScheduleColumns {
    every: Option<i64>,
    unit: Option<&'static str>,
    anchor_on: Option<String>,
    end_on: Option<String>,
    one_off_on: Option<String>,
    goal_start_on: Option<String>,
    goal_due_on: Option<String>,
    goal_completed: Option<bool>,
}

impl ScheduleColumns {
    fn of(schedule: &PlanSchedule) -> Self {
        match *schedule {
            PlanSchedule::Recurring {
                every,
                unit,
                anchor_on,
                end_on,
            } => Self {
                every: Some(i64::from(every)),
                unit: Some(unit.as_str()),
                anchor_on: Some(anchor_on.to_string()),
                end_on: end_on.map(|end_on| end_on.to_string()),
                ..Self::default()
            },
            PlanSchedule::OneOff { on } => Self {
                one_off_on: Some(on.to_string()),
                ..Self::default()
            },
            PlanSchedule::Goal {
                start_on,
                due_on,
                completed,
            } => Self {
                goal_start_on: Some(start_on.to_string()),
                goal_due_on: Some(due_on.to_string()),
                goal_completed: Some(completed),
                ..Self::default()
            },
        }
    }
}

fn schedule_from_row(row: &SqliteRow) -> Result<PlanSchedule, StoreError> {
    fn required<T>(value: Option<T>) -> Result<T, StoreError> {
        value.ok_or_else(|| corrupt("planned item without its schedule"))
    }
    let item_type: PlanItemType = parsed(row, "item_type")?;
    match item_type {
        PlanItemType::Recurring => {
            let every: Option<i64> = row.try_get("every").map_err(StoreError::from_sqlx)?;
            Ok(PlanSchedule::Recurring {
                every: u16::try_from(required(every)?)
                    .map_err(|_| corrupt("planned item interval out of range"))?,
                unit: required(parsed_opt::<IntervalUnit>(row, "unit")?)?,
                anchor_on: required(parsed_opt::<Date>(row, "anchor_on")?)?,
                end_on: parsed_opt(row, "end_on")?,
            })
        }
        PlanItemType::OneOff => Ok(PlanSchedule::OneOff {
            on: required(parsed_opt::<Date>(row, "one_off_on")?)?,
        }),
        PlanItemType::Goal => {
            let completed: Option<bool> = row
                .try_get("goal_completed")
                .map_err(StoreError::from_sqlx)?;
            Ok(PlanSchedule::Goal {
                start_on: required(parsed_opt::<Date>(row, "goal_start_on")?)?,
                due_on: required(parsed_opt::<Date>(row, "goal_due_on")?)?,
                completed: required(completed)?,
            })
        }
    }
}

/// An item row with its history, read as it stands in `month`.
pub(crate) fn from_row(
    row: &SqliteRow,
    history: Vec<PlanVersion>,
    month: CalendarMonth,
) -> Result<PlannedItemRecord, StoreError> {
    let schedule = schedule_from_row(row)?;
    let versions = versions::checked(history, &schedule)?;
    let values = *versions.in_force(month);
    Ok(PlannedItemRecord {
        id: parsed(row, "id")?,
        name: PlannedItemName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        category: values.category,
        amount: values.amount,
        direction: values.direction,
        account_hint: parsed_opt(row, "account_hint_id")?,
        notes: text_opt(row, "notes")?
            .map(|notes| Notes::parse(&notes))
            .transpose()
            .map_err(StoreError::backend)?,
        active: row.try_get("active").map_err(StoreError::from_sqlx)?,
        schedule,
        spending: values.spending,
        tier: values.tier,
        versions,
        created_at: time(row, "created_at")?,
        updated_at: time(row, "updated_at")?,
    })
}

/// Every planned item (only the active ones unless `include_inactive`), by id, with its history
/// read at `month`, in one consistent read of `conn`.
pub(crate) async fn read_all(
    conn: &mut SqliteConnection,
    include_inactive: bool,
    month: CalendarMonth,
) -> Result<Vec<PlannedItemRecord>, StoreError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM planned_items WHERE active = 1 OR ? ORDER BY id"
    ))
    .bind(include_inactive)
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let mut histories = versions::load_all(conn).await?;
    rows.iter()
        .map(|row| {
            let id: PlannedItemId = parsed(row, "id")?;
            from_row(row, histories.remove(&id).unwrap_or_default(), month)
        })
        .collect()
}

async fn fetch(
    conn: &mut SqliteConnection,
    id: PlannedItemId,
    month: CalendarMonth,
) -> Result<PlannedItemRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM planned_items WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let history = versions::load_one(conn, id).await?;
    from_row(&row, history, month)
}

/// The account hint of each of `ids` that exists.
async fn hints_of(
    conn: &mut SqliteConnection,
    ids: &[PlannedItemId],
) -> Result<HashMap<PlannedItemId, Option<AccountId>>, StoreError> {
    let mut hints = HashMap::new();
    if ids.is_empty() {
        return Ok(hints);
    }
    let mut query =
        QueryBuilder::<Sqlite>::new("SELECT id, account_hint_id FROM planned_items WHERE id IN (");
    let mut separated = query.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(")");
    let rows = query
        .build()
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    for row in &rows {
        hints.insert(parsed(row, "id")?, parsed_opt(row, "account_hint_id")?);
    }
    Ok(hints)
}

impl StoreTx {
    /// Create a planned item.
    ///
    /// # Errors
    ///
    /// `Invalid` for values that don't fit the schedule (nothing is written); backend
    /// failures, including a category or hinted account that does not exist.
    pub async fn insert_planned_item(
        &mut self,
        new: NewPlannedItem,
    ) -> Result<PlannedItemRecord, StoreError> {
        let first = PlanVersion {
            seq: 1,
            effective_from: versions::first_month(&new.schedule)?,
            written_on: new.written_on,
            values: PlanValues {
                amount: new.amount,
                direction: new.direction,
                category: new.category,
                spending: new.spending,
                tier: new.tier,
            },
        };
        versions::check_values(&first.values, &new.schedule).map_err(StoreError::Invalid)?;
        let id = PlannedItemId::new();
        let columns = ScheduleColumns::of(&new.schedule);
        sqlx::query(concat!(
            "INSERT INTO planned_items (",
            columns!(),
            ") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(id.to_string())
        .bind(new.name.as_str())
        .bind(new.account_hint.map(|account| account.to_string()))
        .bind(new.notes.as_ref().map(Notes::as_str))
        .bind(new.active)
        .bind(new.schedule.item_type().as_str())
        .bind(columns.every)
        .bind(columns.unit)
        .bind(columns.anchor_on)
        .bind(columns.end_on)
        .bind(columns.one_off_on)
        .bind(columns.goal_start_on)
        .bind(columns.goal_due_on)
        .bind(columns.goal_completed)
        .bind(new.created_at.as_millisecond())
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        versions::insert(&mut self.tx, id, &first).await?;
        Ok(PlannedItemRecord {
            id,
            name: new.name,
            category: new.category,
            amount: new.amount,
            direction: new.direction,
            account_hint: new.account_hint,
            notes: new.notes,
            active: new.active,
            schedule: new.schedule,
            spending: new.spending,
            tier: new.tier,
            versions: PlanVersions::new(vec![first]).map_err(StoreError::backend)?,
            created_at: new.created_at,
            updated_at: new.created_at,
        })
    }

    /// Apply `changes` to a planned item at `at`. The values that change over time are not
    /// here: see [`Self::add_plan_version`].
    ///
    /// # Errors
    ///
    /// `NotFound` if the item does not exist; backend failures, including a
    /// schedule of another type, or a category or hinted account that does
    /// not exist.
    pub async fn update_planned_item(
        &mut self,
        id: PlannedItemId,
        changes: PlannedItemChanges,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE planned_items SET updated_at = ");
        query.push_bind(at.as_millisecond());
        if let Some(name) = &changes.name {
            query.push(", name = ").push_bind(name.as_str());
        }
        if let Some(account_hint) = changes.account_hint {
            query
                .push(", account_hint_id = ")
                .push_bind(account_hint.map(|account| account.to_string()));
        }
        if let Some(notes) = &changes.notes {
            query
                .push(", notes = ")
                .push_bind(notes.as_ref().map(Notes::as_str));
        }
        if let Some(active) = changes.active {
            query.push(", active = ").push_bind(active);
        }
        if let Some(schedule) = &changes.schedule {
            // `item_type` stays as it was, so the table's check refuses
            // another type's columns.
            let columns = ScheduleColumns::of(schedule);
            query
                .push(", every = ")
                .push_bind(columns.every)
                .push(", unit = ")
                .push_bind(columns.unit)
                .push(", anchor_on = ")
                .push_bind(columns.anchor_on)
                .push(", end_on = ")
                .push_bind(columns.end_on)
                .push(", one_off_on = ")
                .push_bind(columns.one_off_on)
                .push(", goal_start_on = ")
                .push_bind(columns.goal_start_on)
                .push(", goal_due_on = ")
                .push_bind(columns.goal_due_on)
                .push(", goal_completed = ")
                .push_bind(columns.goal_completed);
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete a planned item.
    ///
    /// # Errors
    ///
    /// `NotFound` if the item does not exist.
    pub async fn delete_planned_item(&mut self, id: PlannedItemId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM planned_items WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Fetch a planned item as it stands in `month`, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such item.
    pub async fn get_planned_item(
        &mut self,
        id: PlannedItemId,
        month: CalendarMonth,
    ) -> Result<PlannedItemRecord, StoreError> {
        fetch(&mut self.tx, id, month).await
    }

    /// Whether any planned item, inactive ones included, uses `category`, as
    /// seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn category_has_planned_items(
        &mut self,
        category: CategoryId,
    ) -> Result<bool, StoreError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM planned_item_versions WHERE category_id = ?)",
        )
        .bind(category.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(exists)
    }

    /// Whether a split in `category` links to any goal, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn category_has_goal_linked_splits(
        &mut self,
        category: CategoryId,
    ) -> Result<bool, StoreError> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM transaction_splits \
             WHERE category_id = ? AND goal_id IS NOT NULL)",
        )
        .bind(category.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(exists)
    }

    /// Whether any split links to `goal`, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn goal_has_links(&mut self, goal: PlannedItemId) -> Result<bool, StoreError> {
        let (exists,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM transaction_splits WHERE goal_id = ?)")
                .bind(goal.to_string())
                .fetch_one(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
        Ok(exists)
    }

    /// The account hints of `ids`, as [`Store::hints_of_planned_items`], read in
    /// this transaction.
    ///
    /// # Errors
    ///
    /// As [`Store::hints_of_planned_items`].
    pub async fn hints_of_planned_items(
        &mut self,
        ids: &[PlannedItemId],
    ) -> Result<HashMap<PlannedItemId, Option<AccountId>>, StoreError> {
        hints_of(&mut self.tx, ids).await
    }

    /// How many planned items exist, inactive ones included, as seen by this
    /// transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn count_planned_items(&mut self) -> Result<usize, StoreError> {
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM planned_items")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        usize::try_from(count).map_err(StoreError::backend)
    }
}

impl Store {
    /// The account hint of each of `ids` that exists, whichever are active.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn hints_of_planned_items(
        &self,
        ids: &[PlannedItemId],
    ) -> Result<HashMap<PlannedItemId, Option<AccountId>>, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        hints_of(&mut conn, ids).await
    }

    /// Fetch a planned item as it stands in `month`.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such item.
    pub async fn get_planned_item(
        &self,
        id: PlannedItemId,
        month: CalendarMonth,
    ) -> Result<PlannedItemRecord, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        fetch(&mut conn, id, month).await
    }

    /// The planned items as they stand in `month`, ordered by id; inactive ones only when
    /// `include_inactive`.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_planned_items(
        &self,
        include_inactive: bool,
        month: CalendarMonth,
    ) -> Result<Vec<PlannedItemRecord>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        read_all(&mut tx, include_inactive, month).await
    }
}
