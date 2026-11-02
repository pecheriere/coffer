//! What one forecast reads (spec §6.8, §8.9), gathered in one read transaction so
//! that a sync committing midway can't split it.

use std::collections::BTreeMap;

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, PlannedItemId};
use coffer_domain::money::Currency;
use coffer_domain::plan::ForecastSettings;
use coffer_domain::plan_report::{GoalMonth, MonthActual, MonthUncategorized};
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteConnection};

use crate::accounts::account_from_row;
use crate::balances::snapshot_from_row;
use crate::convert::{corrupt, int, parsed, text};
use crate::household::fetch_forecast_settings;
use crate::planned_items::from_row as planned_item_from_row;
use crate::provider_accounts::provider_account_from_row;
use crate::settlements;
use crate::{
    AccountRecord, BalanceSnapshot, ConnectionStatus, PlannedItemRecord, ProviderAccountRecord,
    SettlementRecord, Store, StoreError,
};

/// Everything a forecast reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastData {
    pub settings: ForecastSettings,
    /// Every account, closed ones included, ordered by id, with its latest balance
    /// on or before `as_of`.
    pub accounts: Vec<(AccountRecord, Option<BalanceSnapshot>)>,
    /// The active planned items, ordered by id, so a pool that two items draw on the
    /// same day always pays them in the same order.
    pub items: Vec<PlannedItemRecord>,
    /// Every settlement, ordered by item and month.
    pub settlements: Vec<SettlementRecord>,
    /// Spending linked to each goal, positive for money spent.
    pub goal_spent: Vec<(PlannedItemId, i64)>,
    pub category_spent: Vec<CategorySpentRow>,
    pub loose_ends: Vec<LooseEndsRow>,
    /// Outflows on counted accounts that their balances may not show yet.
    pub newer_spending: Vec<NewerSpendingRow>,
    /// Every provider account, ordered by id.
    pub provider_accounts: Vec<ProviderAccountRecord>,
    /// Every connection, removed ones included, ordered by id.
    pub connections: Vec<ConnectionHealth>,
}

/// A category's spending in `as_of`'s month in one currency, positive for money out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategorySpentRow {
    pub category: CategoryId,
    pub currency: Currency,
    pub spent_minor: i64,
}

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
}

/// What in `as_of`'s month, in one currency, could yet change a category's spending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooseEndsRow {
    pub currency: Currency,
    pub uncategorized_count: u32,
    /// Positive for money out.
    pub uncategorized_minor: i64,
    pub possible_duplicates: u32,
}

/// What counted accounts spent in one currency that their balances may not show yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewerSpendingRow {
    pub currency: Currency,
    /// Posted outflows after the balances' dates, positive for money out.
    pub total_minor: i64,
    /// Pending outflows up to `as_of`, positive for money out.
    pub pending_minor: i64,
}

/// How a connection stands: nothing of its credentials or provider state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHealth {
    pub id: ConnectionId,
    pub label: String,
    pub status: ConnectionStatus,
}

/// Joins each account in `a` to its latest balance on or before the bound `as_of`,
/// in `b`: a balance dated later says nothing about `as_of`.
macro_rules! latest_balance {
    () => {
        "balance_snapshots b ON b.account_id = a.id AND b.on_date = \
         (SELECT MAX(on_date) FROM balance_snapshots \
          WHERE account_id = a.id AND on_date <= ?1)"
    };
}

/// An account `a` of a class a household spends from. Invested and illiquid accounts
/// aren't, so their sweeps and trades don't drown what is.
macro_rules! spendable {
    () => {
        "a.liquidity IN ('cash', 'short_term_liability')"
    };
}

/// Transactions in `t` the balance of an account in the forecast already shows, in
/// the window from the bound month start to the bound `as_of`. The inner join leaves
/// out an account with no balance. A closed account is left out too, though a closed
/// card still owing counts against cash, so its spending doesn't lower the need.
macro_rules! shown_this_month {
    () => {
        concat!(
            "transactions t JOIN accounts a ON a.id = t.account_id JOIN ",
            latest_balance!(),
            " WHERE t.pending = 0 AND t.posted_on >= ?2 AND t.posted_on <= ?1 \
             AND t.posted_on <= b.on_date AND a.closed_at IS NULL \
             AND a.include_in_forecast = 1 AND ",
            spendable!()
        )
    };
}

/// The transaction `t` still says what its splits say: its provider hasn't removed it or
/// revised it away from them.
macro_rules! splits_hold {
    () => {
        "t.flag_source_removed = 0 AND t.flag_split_mismatch = 0"
    };
}

/// A split `s` in a category `c` that adds to what its kind counts: money out of an
/// `expense` category, money into an `income` one.
macro_rules! adds_to_category {
    () => {
        "(c.kind = 'income') = (s.minor_units > 0)"
    };
}

/// Money a category doesn't count (R10a). A charge the provider removed, or revised away
/// from its splits, doesn't say what was spent in a category, so it pays no bill and
/// reduces no budget. An inflow on such a transaction keeps counting: leaving a refund out
/// would raise the spending, the opposite of holding more.
///
/// The same goes for a deposit in an `income` category, which only a report reads: a
/// deposit the provider took back doesn't say what was earned.
macro_rules! left_out_of_category {
    () => {
        concat!("(", adds_to_category!(), " AND NOT (", splits_hold!(), "))")
    };
}

/// Money counted against a goal (R10a): a charge the provider removed or revised away
/// from its splits, or that may be another charge again, doesn't count. An inflow on such
/// a transaction keeps counting, since leaving a refund out would lower the need.
macro_rules! counts_against_goal {
    () => {
        concat!(
            "(s.minor_units > 0 OR (",
            splits_hold!(),
            " AND t.flag_possible_duplicate = 0))"
        )
    };
}

const CATEGORY_SPENT: &str = concat!(
    "SELECT s.category_id AS category, t.currency AS currency, \
     MAX(0, -SUM(s.minor_units)) AS spent_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id, ",
    shown_this_month!(),
    " AND s.transaction_id = t.id AND s.goal_id IS NULL AND c.kind = 'expense' \
     AND NOT ",
    left_out_of_category!(),
    " GROUP BY s.category_id, t.currency ORDER BY s.category_id, t.currency"
);

/// An outflow with no splits at all, in `t`.
macro_rules! uncategorized {
    () => {
        "FILTER (WHERE t.amount_minor < 0 AND NOT EXISTS \
         (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id))"
    };
}

const LOOSE_ENDS: &str = concat!(
    "SELECT t.currency AS currency, COUNT(*) ",
    uncategorized!(),
    " AS uncategorized_count, COALESCE(-SUM(t.amount_minor) ",
    uncategorized!(),
    ", 0) AS uncategorized_minor, SUM(t.flag_possible_duplicate) AS possible_duplicates FROM ",
    shown_this_month!(),
    " GROUP BY t.currency HAVING uncategorized_count > 0 OR possible_duplicates > 0 \
     ORDER BY t.currency"
);

/// Spending linked to each goal: `expense` splits only, since money moved to savings
/// for a goal is still cash.
///
/// The balance date limits an account switched in and of a spendable class, open or a
/// closed card: spending newer than the balance isn't in it yet, an account with no
/// balance shows none, and a closed card's debt may be what the balance hasn't reached.
/// Spending from any other account never touched counted cash.
const GOAL_SPENT: &str = concat!(
    "SELECT s.goal_id AS goal, MAX(0, -SUM(s.minor_units)) AS spent_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     JOIN accounts a ON a.id = t.account_id LEFT JOIN ",
    latest_balance!(),
    " WHERE s.goal_id IS NOT NULL AND c.kind = 'expense' AND t.pending = 0 \
       AND ",
    counts_against_goal!(),
    " AND t.posted_on <= ?1 AND ( \
       NOT (a.include_in_forecast = 1 AND ",
    spendable!(),
    " AND (a.closed_at IS NULL OR a.liquidity = 'short_term_liability')) \
       OR t.posted_on <= COALESCE(b.on_date, '') \
     ) GROUP BY s.goal_id ORDER BY s.goal_id"
);

/// Outflows up to `as_of` that a balance may not show, on an account switched in and of a
/// spendable class: posted after the date of the balance the forecast takes, or pending
/// on any date, which is either one or the other. An open account has to have a balance,
/// since without one the forecast is blocked already. A closed card doesn't, since closing
/// it settles no debt its balance doesn't show. Closed cash isn't in the cash.
const NEWER_SPENDING: &str = concat!(
    "SELECT t.currency AS currency, \
     COALESCE(-SUM(t.amount_minor) FILTER (WHERE t.pending = 0), 0) AS total_minor, \
     COALESCE(-SUM(t.amount_minor) FILTER (WHERE t.pending = 1), 0) AS pending_minor \
     FROM transactions t JOIN accounts a ON a.id = t.account_id LEFT JOIN ",
    latest_balance!(),
    " WHERE t.amount_minor < 0 AND t.posted_on <= ?1 \
       AND (t.pending = 1 OR t.posted_on > COALESCE(b.on_date, '')) \
       AND a.include_in_forecast = 1 AND ",
    spendable!(),
    " AND ((a.closed_at IS NULL AND b.on_date IS NOT NULL) \
            OR (a.closed_at IS NOT NULL AND a.liquidity = 'short_term_liability')) \
     GROUP BY t.currency ORDER BY t.currency"
);

/// What a report takes as actual: posted splits in the window on any account, since it
/// says what happened rather than what a balance shows. A goal's are on its own line.
const REPORT_ACTUALS: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, s.category_id AS category, c.kind AS kind, \
     t.currency AS currency, \
     COALESCE(SUM(s.minor_units) FILTER (WHERE NOT ",
    left_out_of_category!(),
    "), 0) AS net_minor, \
     COALESCE(ABS(SUM(s.minor_units) FILTER (WHERE ",
    left_out_of_category!(),
    ")), 0) AS left_out_minor, \
     COALESCE(ABS(SUM(s.minor_units) FILTER (WHERE t.flag_possible_duplicate = 1 AND ",
    adds_to_category!(),
    " AND ",
    splits_hold!(),
    ")), 0) AS possible_duplicate_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     WHERE s.goal_id IS NULL AND t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 \
     GROUP BY month, s.category_id, c.kind, t.currency \
     ORDER BY month, s.category_id, t.currency"
);

/// What is linked to each goal by month, on any account: `expense` splits only, as in
/// [`GOAL_SPENT`].
const REPORT_GOAL_SPENT: &str = concat!(
    "SELECT s.goal_id AS goal, substr(t.posted_on, 1, 7) AS month, \
     COALESCE(-SUM(s.minor_units) FILTER (WHERE ",
    counts_against_goal!(),
    "), 0) AS spent_minor, \
     COALESCE(-SUM(s.minor_units) FILTER (WHERE NOT ",
    counts_against_goal!(),
    "), 0) AS left_out_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id \
     JOIN transactions t ON t.id = s.transaction_id \
     WHERE s.goal_id IS NOT NULL AND c.kind = 'expense' AND t.pending = 0 \
     AND t.posted_on >= ?1 AND t.posted_on < ?2 \
     GROUP BY s.goal_id, month ORDER BY s.goal_id, month"
);

/// Uncategorized outflows by month, as the forecast's loose ends count them, on any
/// spendable account: a trade or a sweep elsewhere is never spending (R6).
const REPORT_UNCATEGORIZED: &str = concat!(
    "SELECT substr(t.posted_on, 1, 7) AS month, t.currency AS currency, COUNT(*) ",
    uncategorized!(),
    " AS uncategorized_count, COALESCE(-SUM(t.amount_minor) ",
    uncategorized!(),
    ", 0) AS uncategorized_minor \
     FROM transactions t JOIN accounts a ON a.id = t.account_id \
     WHERE t.pending = 0 AND t.posted_on >= ?1 AND t.posted_on < ?2 AND ",
    spendable!(),
    " GROUP BY month, t.currency HAVING uncategorized_count > 0 \
     ORDER BY month, t.currency"
);

/// A summed amount. `SUM` that overflows fails its statement, but negating a sum of exactly
/// `i64::MIN` makes it a float instead, which is the same sum too large for an integer.
fn summed(row: &SqliteRow, column: &str) -> Result<i64, StoreError> {
    row.try_get(column).map_err(|err| match err {
        sqlx::Error::ColumnDecode { .. } => StoreError::SumTooLarge,
        other => StoreError::from_sqlx(other),
    })
}

fn count(row: &SqliteRow, column: &str) -> Result<u32, StoreError> {
    u32::try_from(int(row, column)?).map_err(|_| corrupt("a count out of range"))
}

fn category_spent_from_row(row: &SqliteRow) -> Result<CategorySpentRow, StoreError> {
    Ok(CategorySpentRow {
        category: parsed(row, "category")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        spent_minor: summed(row, "spent_minor")?,
    })
}

fn loose_ends_from_row(row: &SqliteRow) -> Result<LooseEndsRow, StoreError> {
    Ok(LooseEndsRow {
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        uncategorized_count: count(row, "uncategorized_count")?,
        uncategorized_minor: summed(row, "uncategorized_minor")?,
        possible_duplicates: count(row, "possible_duplicates")?,
    })
}

fn newer_spending_from_row(row: &SqliteRow) -> Result<NewerSpendingRow, StoreError> {
    Ok(NewerSpendingRow {
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        total_minor: summed(row, "total_minor")?,
        pending_minor: summed(row, "pending_minor")?,
    })
}

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

fn connection_health_from_row(row: &SqliteRow) -> Result<ConnectionHealth, StoreError> {
    let label = ConnectionLabel::parse(&text(row, "label")?).map_err(StoreError::backend)?;
    Ok(ConnectionHealth {
        id: parsed(row, "id")?,
        label: label.as_str().to_owned(),
        status: parsed(row, "status")?,
    })
}

/// Each account with its latest balance on or before `as_of`.
async fn accounts_with_balances(
    conn: &mut SqliteConnection,
    as_of: &str,
) -> Result<Vec<(AccountRecord, Option<BalanceSnapshot>)>, StoreError> {
    let accounts = sqlx::query("SELECT * FROM accounts ORDER BY id")
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let balances = sqlx::query(
        "SELECT b.* FROM balance_snapshots b WHERE b.on_date = \
         (SELECT MAX(on_date) FROM balance_snapshots \
          WHERE account_id = b.account_id AND on_date <= ?)",
    )
    .bind(as_of)
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let mut latest: BTreeMap<AccountId, BalanceSnapshot> = BTreeMap::new();
    for row in &balances {
        let snapshot = snapshot_from_row(row)?;
        latest.insert(snapshot.account, snapshot);
    }
    accounts
        .iter()
        .map(|row| {
            let account = account_from_row(row)?;
            let balance = latest.remove(&account.id);
            Ok((account, balance))
        })
        .collect()
}

impl Store {
    /// Everything a forecast from `as_of` reads, seen at one moment.
    ///
    /// # Errors
    ///
    /// [`StoreError::SumTooLarge`] for a sum too large for the database; backend
    /// failures, including a row that no longer parses.
    pub async fn forecast_data(&self, as_of: Date) -> Result<ForecastData, StoreError> {
        let day = as_of.to_string();
        let month_start = as_of.first_of_month().to_string();
        // One read transaction: a sync committing midway can't split a forecast.
        let mut tx = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let settings = fetch_forecast_settings(&mut *tx).await?;
        let accounts = accounts_with_balances(&mut tx, &day).await?;
        let items = sqlx::query("SELECT * FROM planned_items WHERE active = 1 ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(planned_item_from_row)
            .collect::<Result<_, _>>()?;
        let settlements = settlements::all(&mut tx).await?;
        let goal_spent = sqlx::query(GOAL_SPENT)
            .bind(&day)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(|row| Ok((parsed(row, "goal")?, summed(row, "spent_minor")?)))
            .collect::<Result<_, StoreError>>()?;
        let category_spent = sqlx::query(CATEGORY_SPENT)
            .bind(&day)
            .bind(&month_start)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(category_spent_from_row)
            .collect::<Result<_, _>>()?;
        let loose_ends = sqlx::query(LOOSE_ENDS)
            .bind(&day)
            .bind(&month_start)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(loose_ends_from_row)
            .collect::<Result<_, _>>()?;
        let newer_spending = sqlx::query(NEWER_SPENDING)
            .bind(&day)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(newer_spending_from_row)
            .collect::<Result<_, _>>()?;
        let provider_accounts = sqlx::query("SELECT * FROM provider_accounts ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(provider_account_from_row)
            .collect::<Result<_, _>>()?;
        let connections = sqlx::query("SELECT id, label, status FROM connections ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(connection_health_from_row)
            .collect::<Result<_, _>>()?;
        tx.commit().await.map_err(StoreError::from_sqlx)?;
        Ok(ForecastData {
            settings,
            accounts,
            items,
            settlements,
            goal_spent,
            category_spent,
            loose_ends,
            newer_spending,
            provider_accounts,
            connections,
        })
    }

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
        tx.commit().await.map_err(StoreError::from_sqlx)?;
        Ok(PlanReportData {
            items,
            settlements,
            actuals,
            goal_spent,
            uncategorized,
        })
    }
}
