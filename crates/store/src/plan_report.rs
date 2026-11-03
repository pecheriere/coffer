//! What a plan versus actual report reads (spec §6.9), gathered in one read transaction so
//! that a sync committing midway can't split it.

use coffer_domain::fx::RateBook;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::money::Currency;
use coffer_domain::plan_report::{
    GoalMonth, MonthActual, MonthMoved, MonthNotHousehold, MonthUncategorized,
};
use coffer_domain::transfer_rate::CrossCurrencyLink;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, text};
use crate::planned_items::from_row as planned_item_from_row;
use crate::settlements;
use crate::spending_sql::{
    adds_to_category, count, counts_against_goal, left_out_of_category, spendable, splits_hold,
    summed, the_households, uncategorized,
};
use crate::transfer_links::{
    after_moved, both_counted_when_posted, counted_when_posted, link_amount, link_legs,
    link_stands, moved_minor,
};
use crate::transfer_rates;
use crate::{PlannedItemRecord, SettlementRecord, Store, StoreError};

/// Everything a plan versus actual report reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReportData {
    /// Every planned item, inactive ones too, ordered by id.
    pub items: Vec<PlannedItemRecord>,
    /// Every settlement, ordered by item and month.
    pub settlements: Vec<SettlementRecord>,
    /// Per month, category and currency, ordered by them.
    pub actuals: Vec<MonthActual>,
    /// Per goal and month, ordered by them.
    pub goal_spent: Vec<(PlannedItemId, GoalMonth)>,
    /// Per month and currency, ordered by them.
    pub uncategorized: Vec<MonthUncategorized>,
    /// Per month and currency, ordered by them.
    pub not_household: Vec<MonthNotHousehold>,
    /// Per month and currency, ordered by them.
    pub moved: Vec<MonthMoved>,
    /// The links that were between counted accounts in two currencies with a leg in the
    /// window, ordered by id.
    pub cross_currency_links: Vec<CrossCurrencyLink>,
    /// The rates to compare their legs at.
    pub rates: RateBook,
}

/// What a report takes as actual: posted splits in the window on any of the household's
/// accounts, counted or not, since it says what happened rather than what a balance shows.
/// A goal's are on its own line.
const REPORT_ACTUALS: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, s.category_id AS category, c.kind AS kind, \
     t.currency AS currency, \
     COALESCE(SUM(",
    after_moved!("s.minor_units", both_counted_when_posted),
    ") FILTER (WHERE NOT ",
    left_out_of_category!(),
    "), 0) AS net_minor, \
     COALESCE(ABS(SUM(",
    after_moved!("s.minor_units", both_counted_when_posted),
    ") FILTER (WHERE ",
    left_out_of_category!(),
    ")), 0) AS left_out_minor, \
     COALESCE(ABS(SUM(",
    after_moved!("s.minor_units", both_counted_when_posted),
    ") FILTER (WHERE t.flag_possible_duplicate = 1 AND ",
    adds_to_category!(),
    " AND ",
    splits_hold!(),
    ")), 0) AS possible_duplicate_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     JOIN accounts a ON a.id = t.account_id \
     WHERE s.goal_id IS NULL AND t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 \
     AND ",
    the_households!(),
    " GROUP BY month, s.category_id, c.kind, t.currency \
     ORDER BY month, s.category_id, t.currency"
);

/// What [`REPORT_ACTUALS`] leaves out for being on an account that isn't the household's:
/// posted splits in `expense` and `income` categories linked to no goal, by month. Flags
/// are ignored, since it says what is left out, not what it meant.
const REPORT_NOT_HOUSEHOLD: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, t.currency AS currency, \
     SUM(s.minor_units) AS net_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     JOIN accounts a ON a.id = t.account_id \
     WHERE s.goal_id IS NULL AND t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 \
     AND c.kind IN ('expense', 'income') AND NOT ",
    the_households!(),
    " GROUP BY month, t.currency ORDER BY month, t.currency"
);

/// What [`REPORT_ACTUALS`] leaves out for being one movement, by month: what moved of every
/// posted leg, whatever it is filed as.
const REPORT_MOVED: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, t.currency AS currency, \
     COALESCE(SUM(",
    moved_minor!(both_counted_when_posted),
    ") FILTER (WHERE t.amount_minor < 0), 0) AS out_minor, \
     COALESCE(SUM(",
    moved_minor!(both_counted_when_posted),
    ") FILTER (WHERE t.amount_minor > 0), 0) AS in_minor \
     FROM transactions t \
     WHERE t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 AND ",
    moved_minor!(both_counted_when_posted),
    " > 0 GROUP BY month, t.currency ORDER BY month, t.currency"
);

/// What is linked to each goal by month, on any account: `expense` splits only, as in
/// [`GOAL_SPENT`]. A charge that moved is left out as one the flags keep out is. A refund that
/// moved was never a refund, so it just isn't counted, and `left_out` stays charges only.
const REPORT_GOAL_SPENT: &str = concat!(
    "SELECT s.goal_id AS goal, substr(t.posted_on, 1, 7) AS month, \
     COALESCE(-SUM(",
    after_moved!("s.minor_units", both_counted_when_posted),
    ") FILTER (WHERE ",
    counts_against_goal!(),
    "), 0) AS spent_minor, \
     COALESCE(-SUM(s.minor_units) FILTER (WHERE NOT ",
    counts_against_goal!(),
    "), 0) + COALESCE(-SUM(s.minor_units - ",
    after_moved!("s.minor_units", both_counted_when_posted),
    ") FILTER (WHERE s.minor_units < 0 AND ",
    counts_against_goal!(),
    "), 0) AS left_out_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     WHERE s.goal_id IS NOT NULL AND c.kind = 'expense' AND t.pending = 0 \
     AND t.posted_on >= ?1 AND t.posted_on < ?2 \
     GROUP BY s.goal_id, month ORDER BY s.goal_id, month"
);

/// Uncategorized outflows by month, as the forecast's loose ends count them, on any
/// spendable account of the household's: a trade or a sweep elsewhere is never spending (R6),
/// and nor is what happens on money that isn't the household's (spec §8.11 B10).
const REPORT_UNCATEGORIZED: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, t.currency AS currency, COUNT(*) ",
    uncategorized!(both_counted_when_posted),
    " AS uncategorized_count, COALESCE(-SUM(",
    after_moved!("t.amount_minor", both_counted_when_posted),
    ") ",
    uncategorized!(both_counted_when_posted),
    ", 0) AS uncategorized_minor \
     FROM transactions t JOIN accounts a ON a.id = t.account_id \
     WHERE t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 AND ",
    the_households!(),
    " AND ",
    spendable!(),
    " GROUP BY month, t.currency HAVING uncategorized_count > 0 \
     ORDER BY month, t.currency"
);

fn month_actual_from_row(row: &SqliteRow) -> Result<MonthActual, StoreError> {
    Ok(MonthActual {
        month: parsed(row, "month")?,
        category: parsed(row, "category")?,
        kind: parsed(row, "kind")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        net_minor: summed(row, "net_minor")?,
        left_out_minor: summed(row, "left_out_minor")?,
        possible_duplicate_minor: summed(row, "possible_duplicate_minor")?,
    })
}

fn goal_month_from_row(row: &SqliteRow) -> Result<(PlannedItemId, GoalMonth), StoreError> {
    Ok((
        parsed(row, "goal")?,
        GoalMonth {
            month: parsed(row, "month")?,
            spent_minor: summed(row, "spent_minor")?,
            left_out_minor: summed(row, "left_out_minor")?,
        },
    ))
}

fn month_uncategorized_from_row(row: &SqliteRow) -> Result<MonthUncategorized, StoreError> {
    Ok(MonthUncategorized {
        month: parsed(row, "month")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        count: count(row, "uncategorized_count")?,
        total_minor: summed(row, "uncategorized_minor")?,
    })
}

fn month_not_household_from_row(row: &SqliteRow) -> Result<MonthNotHousehold, StoreError> {
    Ok(MonthNotHousehold {
        month: parsed(row, "month")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        net_minor: summed(row, "net_minor")?,
    })
}

fn month_moved_from_row(row: &SqliteRow) -> Result<MonthMoved, StoreError> {
    Ok(MonthMoved {
        month: parsed(row, "month")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        out_minor: summed(row, "out_minor")?,
        in_minor: summed(row, "in_minor")?,
    })
}

impl Store {
    /// Everything a plan versus actual report over `[from, to_exclusive)` reads, seen at
    /// one moment.
    ///
    /// # Errors
    ///
    /// [`StoreError::SumTooLarge`] for a sum too large for the database; backend
    /// failures, including a row that no longer parses.
    pub async fn plan_report_data(
        &self,
        from: Date,
        to_exclusive: Date,
    ) -> Result<PlanReportData, StoreError> {
        let (from_date, to_date) = (from, to_exclusive);
        let (from, to_exclusive) = (from.to_string(), to_exclusive.to_string());
        // One read transaction: a sync committing midway can't split a report.
        let mut tx = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let items = sqlx::query("SELECT * FROM planned_items ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(planned_item_from_row)
            .collect::<Result<_, _>>()?;
        let settlements = settlements::all(&mut tx).await?;
        let actuals = sqlx::query(REPORT_ACTUALS)
            .bind(&from)
            .bind(&to_exclusive)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(month_actual_from_row)
            .collect::<Result<_, _>>()?;
        let goal_spent = sqlx::query(REPORT_GOAL_SPENT)
            .bind(&from)
            .bind(&to_exclusive)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(goal_month_from_row)
            .collect::<Result<_, _>>()?;
        let uncategorized = sqlx::query(REPORT_UNCATEGORIZED)
            .bind(&from)
            .bind(&to_exclusive)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(month_uncategorized_from_row)
            .collect::<Result<_, _>>()?;
        let not_household = sqlx::query(REPORT_NOT_HOUSEHOLD)
            .bind(&from)
            .bind(&to_exclusive)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(month_not_household_from_row)
            .collect::<Result<_, _>>()?;
        let moved = sqlx::query(REPORT_MOVED)
            .bind(&from)
            .bind(&to_exclusive)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(month_moved_from_row)
            .collect::<Result<_, _>>()?;
        let (cross_currency_links, rates) =
            transfer_rates::for_report(&mut tx, from_date, to_date).await?;
        tx.commit().await.map_err(StoreError::from_sqlx)?;
        Ok(PlanReportData {
            items,
            settlements,
            actuals,
            goal_spent,
            uncategorized,
            not_household,
            moved,
            cross_currency_links,
            rates,
        })
    }
}
