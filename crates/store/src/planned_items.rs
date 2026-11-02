//! Planned items (spec §6.7): what the household expects to spend or receive.
//! Every value read is parsed back into its domain type, so a row that no
//! longer fits is a backend error.

use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{
    IntervalUnit, PlanDirection, PlanItemType, PlanSchedule, PlanSpending, PlannedItemName,
    spending_applies, validate_amount,
};
use coffer_domain::transactions::Notes;
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, name, category_id, amount_minor, currency, direction, account_hint_id, notes, \
         active, item_type, every, unit, anchor_on, end_on, one_off_on, goal_start_on, \
         goal_due_on, goal_completed, spending, created_at, updated_at"
    };
}

/// A stored planned item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItemRecord {
    pub id: PlannedItemId,
    pub name: PlannedItemName,
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
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
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
    pub created_at: Timestamp,
}

/// The fields of a planned item to change; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlannedItemChanges {
    pub name: Option<PlannedItemName>,
    pub category: Option<CategoryId>,
    pub amount: Option<Money>,
    pub direction: Option<PlanDirection>,
    /// `Some(None)` clears the hint.
    pub account_hint: Option<Option<AccountId>>,
    /// `Some(None)` clears the notes.
    pub notes: Option<Option<Notes>>,
    pub active: Option<bool>,
    /// A schedule of the item's own type: the table refuses any other.
    pub schedule: Option<PlanSchedule>,
    /// `Some(None)` clears it.
    pub spending: Option<Option<PlanSpending>>,
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

fn from_row(row: &SqliteRow) -> Result<PlannedItemRecord, StoreError> {
    let amount = Money {
        minor_units: row.try_get("amount_minor").map_err(StoreError::from_sqlx)?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
    };
    validate_amount(amount).map_err(StoreError::backend)?;
    let direction = parsed(row, "direction")?;
    let schedule = schedule_from_row(row)?;
    schedule.validate(direction).map_err(StoreError::backend)?;
    let spending = parsed_opt(row, "spending")?;
    if spending.is_some() != spending_applies(direction, &schedule) {
        return Err(corrupt("planned item spending does not fit its type"));
    }
    Ok(PlannedItemRecord {
        id: parsed(row, "id")?,
        name: PlannedItemName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        category: parsed(row, "category_id")?,
        amount,
        direction,
        account_hint: parsed_opt(row, "account_hint_id")?,
        notes: text_opt(row, "notes")?
            .map(|notes| Notes::parse(&notes))
            .transpose()
            .map_err(StoreError::backend)?,
        active: row.try_get("active").map_err(StoreError::from_sqlx)?,
        schedule,
        spending,
        created_at: time(row, "created_at")?,
        updated_at: time(row, "updated_at")?,
    })
}

async fn fetch<'e>(
    executor: impl SqliteExecutor<'e>,
    id: PlannedItemId,
) -> Result<PlannedItemRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM planned_items WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    from_row(&row)
}

impl StoreTx {
    /// Create a planned item.
    ///
    /// # Errors
    ///
    /// Backend failures, including a category or hinted account that does not
    /// exist.
    pub async fn insert_planned_item(
        &mut self,
        new: NewPlannedItem,
    ) -> Result<PlannedItemRecord, StoreError> {
        let id = PlannedItemId::new();
        let columns = ScheduleColumns::of(&new.schedule);
        sqlx::query(concat!(
            "INSERT INTO planned_items (",
            columns!(),
            ") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(id.to_string())
        .bind(new.name.as_str())
        .bind(new.category.to_string())
        .bind(new.amount.minor_units)
        .bind(new.amount.currency.as_str())
        .bind(new.direction.as_str())
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
        .bind(new.spending.map(PlanSpending::as_str))
        .bind(new.created_at.as_millisecond())
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
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
            created_at: new.created_at,
            updated_at: new.created_at,
        })
    }

    /// Apply `changes` to a planned item at `at` and return it as it now is.
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
    ) -> Result<PlannedItemRecord, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE planned_items SET updated_at = ");
        query.push_bind(at.as_millisecond());
        if let Some(name) = &changes.name {
            query.push(", name = ").push_bind(name.as_str());
        }
        if let Some(category) = changes.category {
            query
                .push(", category_id = ")
                .push_bind(category.to_string());
        }
        if let Some(amount) = changes.amount {
            query
                .push(", amount_minor = ")
                .push_bind(amount.minor_units)
                .push(", currency = ")
                .push_bind(amount.currency.as_str());
        }
        if let Some(direction) = changes.direction {
            query.push(", direction = ").push_bind(direction.as_str());
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
        if let Some(spending) = changes.spending {
            query
                .push(", spending = ")
                .push_bind(spending.map(PlanSpending::as_str));
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch(&mut *self.tx, id).await
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

    /// Fetch a planned item, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such item.
    pub async fn get_planned_item(
        &mut self,
        id: PlannedItemId,
    ) -> Result<PlannedItemRecord, StoreError> {
        fetch(&mut *self.tx, id).await
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
    /// Fetch a planned item.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such item.
    pub async fn get_planned_item(
        &self,
        id: PlannedItemId,
    ) -> Result<PlannedItemRecord, StoreError> {
        fetch(&self.pool, id).await
    }

    /// The planned items, ordered by id; inactive ones only when
    /// `include_inactive`.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_planned_items(
        &self,
        include_inactive: bool,
    ) -> Result<Vec<PlannedItemRecord>, StoreError> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM planned_items WHERE active = 1 OR ? ORDER BY id"
        ))
        .bind(include_inactive)
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(from_row).collect()
    }
}
