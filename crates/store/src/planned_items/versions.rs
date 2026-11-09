//! The versions of a planned item (money redesign §10.3): its amount, direction, category,
//! spending kind and tier by month. A version is only ever appended, and only from this month
//! or next, so the store has no way to change what a past month reads.

use std::collections::HashMap;

use coffer_domain::ids::{CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{
    CalendarMonth, EffectiveFrom, PlanDirection, PlanError, PlanItemType, PlanSchedule, PlanTier,
    PlanValues, PlanVersion, PlanVersions, spending_applies, validate_amount,
};
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use super::schedule_from_row;
use crate::convert::{corrupt, parsed, parsed_opt, text};
use crate::{StoreError, StoreTx};

macro_rules! columns {
    () => {
        "item_id, seq, effective_from, written_on, amount_minor, currency, direction, \
         category_id, spending, tier"
    };
}

/// A row of `planned_item_versions` as the item it belongs to and the version it holds.
fn version_from_row(row: &SqliteRow) -> Result<(PlannedItemId, PlanVersion), StoreError> {
    let amount = Money {
        minor_units: row.try_get("amount_minor").map_err(StoreError::from_sqlx)?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
    };
    let seq: i64 = row.try_get("seq").map_err(StoreError::from_sqlx)?;
    let version = PlanVersion {
        seq: u32::try_from(seq).map_err(|_| corrupt("plan version number out of range"))?,
        effective_from: parsed(row, "effective_from")?,
        written_on: parsed::<Date>(row, "written_on")?,
        values: PlanValues {
            amount,
            direction: parsed(row, "direction")?,
            category: parsed::<CategoryId>(row, "category_id")?,
            spending: parsed_opt(row, "spending")?,
            tier: parsed_opt(row, "tier")?,
        },
    };
    Ok((parsed(row, "item_id")?, version))
}

/// The history of every item, by item.
pub(super) async fn load_all(
    conn: &mut SqliteConnection,
) -> Result<HashMap<PlannedItemId, Vec<PlanVersion>>, StoreError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM planned_item_versions ORDER BY item_id, seq"
    ))
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let mut by_item: HashMap<PlannedItemId, Vec<PlanVersion>> = HashMap::new();
    for row in &rows {
        let (item, version) = version_from_row(row)?;
        by_item.entry(item).or_default().push(version);
    }
    Ok(by_item)
}

/// The history of one item.
pub(super) async fn load_one(
    conn: &mut SqliteConnection,
    id: PlannedItemId,
) -> Result<Vec<PlanVersion>, StoreError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM planned_item_versions WHERE item_id = ? ORDER BY seq"
    ))
    .bind(id.to_string())
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    rows.iter()
        .map(|row| version_from_row(row).map(|(_, version)| version))
        .collect()
}

/// The history as a whole, refusing one that is empty or any row that doesn't fit the item's
/// schedule, as the table's own checks can't see the item's type.
pub(super) fn checked(
    rows: Vec<PlanVersion>,
    schedule: &PlanSchedule,
) -> Result<PlanVersions, StoreError> {
    for row in &rows {
        check_values(&row.values, schedule).map_err(corrupt)?;
    }
    PlanVersions::new(rows).map_err(StoreError::backend)
}

/// Whether `values` fit an item on `schedule`: an amount in range, a spending kind on a
/// recurring outflow only, a tier on an outflow only and saving toward on a goal only.
fn check_values(values: &PlanValues, schedule: &PlanSchedule) -> Result<(), &'static str> {
    validate_amount(values.amount).map_err(|_| "plan version amount out of range")?;
    schedule
        .validate(values.direction)
        .map_err(|_| "plan version direction does not fit the schedule")?;
    if values.spending.is_some() != spending_applies(values.direction, schedule) {
        return Err("plan version spending does not fit the item's type");
    }
    let goal = schedule.item_type() == PlanItemType::Goal;
    if values.tier.is_some() != (values.direction == PlanDirection::Outflow) {
        return Err("plan version tier does not fit its direction");
    }
    if values.tier.is_some() && (values.tier == Some(PlanTier::SavingToward)) != goal {
        return Err("plan version tier does not fit the item's type");
    }
    Ok(())
}

fn invalid(error: PlanError) -> StoreError {
    match error {
        PlanError::PastVersion => {
            StoreError::Invalid("only this month's or next month's plan can change")
        }
        PlanError::NextMonthNeedsRecurring => {
            StoreError::Invalid("only a recurring item can change from next month")
        }
        other => StoreError::backend(other),
    }
}

/// The month an item's first version takes effect from: that of its anchor.
pub(super) fn first_month(schedule: &PlanSchedule) -> Result<CalendarMonth, StoreError> {
    let day = match *schedule {
        PlanSchedule::Recurring { anchor_on, .. } => anchor_on,
        PlanSchedule::OneOff { on } => on,
        PlanSchedule::Goal { start_on, .. } => start_on,
    };
    CalendarMonth::of(day).map_err(StoreError::backend)
}

pub(super) async fn insert(
    conn: &mut SqliteConnection,
    item: PlannedItemId,
    version: &PlanVersion,
) -> Result<(), StoreError> {
    let values = &version.values;
    sqlx::query(
        "INSERT INTO planned_item_versions (item_id, seq, effective_from, written_on, \
         amount_minor, currency, direction, category_id, spending, tier) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.to_string())
    .bind(i64::from(version.seq))
    .bind(version.effective_from.to_string())
    .bind(version.written_on.to_string())
    .bind(values.amount.minor_units)
    .bind(values.amount.currency.as_str())
    .bind(values.direction.as_str())
    .bind(values.category.to_string())
    .bind(
        values
            .spending
            .map(coffer_domain::plan::PlanSpending::as_str),
    )
    .bind(values.tier.map(PlanTier::as_str))
    .execute(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    Ok(())
}

impl StoreTx {
    /// Append a version of a planned item's values, effective from this month or next as
    /// `from` says (`today` being the household's day). A version planned from next month and
    /// not yet in force is replaced for that month by this one. There is no way to write, change
    /// or delete a version for a past month.
    ///
    /// # Errors
    ///
    /// `NotFound` if the item does not exist; `Invalid` for a month that is not this one or
    /// next, next month for an item that isn't recurring, or values that don't fit the item's
    /// schedule; backend failures, including a
    /// category that does not exist.
    pub async fn add_plan_version(
        &mut self,
        id: PlannedItemId,
        today: Date,
        from: EffectiveFrom,
        values: PlanValues,
    ) -> Result<PlanVersion, StoreError> {
        let row = sqlx::query(
            "SELECT item_type, every, unit, anchor_on, end_on, one_off_on, goal_start_on, \
             goal_due_on, goal_completed FROM planned_items WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?
        .ok_or(StoreError::NotFound)?;
        let schedule = schedule_from_row(&row)?;
        let effective_from = from.resolve(today, schedule.item_type()).map_err(invalid)?;
        check_values(&values, &schedule).map_err(StoreError::Invalid)?;
        let (next,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM planned_item_versions WHERE item_id = ?",
        )
        .bind(id.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let version = PlanVersion {
            seq: u32::try_from(next).map_err(|_| corrupt("plan version number out of range"))?,
            effective_from,
            written_on: today,
            values,
        };
        insert(&mut self.tx, id, &version).await?;
        Ok(version)
    }
}
