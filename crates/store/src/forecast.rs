//! What one forecast reads (spec §6.8, §8.9), gathered in one read transaction so
//! that a sync committing midway can't split it.

use std::collections::BTreeMap;

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, PlannedItemId};
use coffer_domain::money::Currency;
use coffer_domain::plan::ForecastSettings;
use jiff::civil::Date;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteRow;

use crate::accounts::account_from_row;
use crate::balances::snapshot_from_row;
use crate::convert::{corrupt, int, parsed, text};
use crate::household::fetch_forecast_settings;
use crate::planned_items::from_row as planned_item_from_row;
use crate::provider_accounts::provider_account_from_row;
use crate::{
    AccountRecord, BalanceSnapshot, ConnectionStatus, PlannedItemRecord, ProviderAccountRecord,
    Store, StoreError,
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
    /// Spending linked to each goal, positive for money spent.
    pub goal_spent: Vec<(PlannedItemId, i64)>,
    pub category_spent: Vec<CategorySpentRow>,
    pub loose_ends: Vec<LooseEndsRow>,
    /// Outflows on counted accounts after their balances' dates, up to `as_of`.
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

/// What in `as_of`'s month, in one currency, could yet change a category's spending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooseEndsRow {
    pub currency: Currency,
    pub uncategorized_count: u32,
    /// Positive for money out.
    pub uncategorized_minor: i64,
    pub possible_duplicates: u32,
}

/// What counted accounts spent in one currency after their balances' dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewerSpendingRow {
    pub currency: Currency,
    /// Positive for money out.
    pub total_minor: i64,
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
             AND a.include_in_forecast = 1 AND a.liquidity IN ('cash', 'short_term_liability')"
        )
    };
}

/// A charge the provider removed, or revised away from its splits, doesn't say what
/// was spent in a category, so it pays no bill and reduces no budget.
const CATEGORY_SPENT: &str = concat!(
    "SELECT s.category_id AS category, t.currency AS currency, \
     MAX(0, -SUM(s.minor_units)) AS spent_minor \
     FROM transaction_splits s JOIN categories c ON c.id = s.category_id, ",
    shown_this_month!(),
    " AND s.transaction_id = t.id AND s.goal_id IS NULL AND c.kind = 'expense' \
     AND t.flag_source_removed = 0 AND t.flag_split_mismatch = 0 \
     GROUP BY s.category_id, t.currency ORDER BY s.category_id, t.currency"
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
/// for a goal is still cash. A charge the provider removed or revised away from its
/// splits, or that may be another charge again, doesn't lower the need.
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
       AND t.flag_source_removed = 0 AND t.flag_split_mismatch = 0 \
       AND t.flag_possible_duplicate = 0 AND t.posted_on <= ?1 AND ( \
       NOT (a.include_in_forecast = 1 AND a.liquidity IN ('cash', 'short_term_liability') \
            AND (a.closed_at IS NULL OR a.liquidity = 'short_term_liability')) \
       OR t.posted_on <= COALESCE(b.on_date, '') \
     ) GROUP BY s.goal_id ORDER BY s.goal_id"
);

/// Posted outflows on accounts the forecast counts, as it counts them, after the date
/// of the balance it takes for them, up to `as_of`.
const NEWER_SPENDING: &str = concat!(
    "SELECT t.currency AS currency, -SUM(t.amount_minor) AS total_minor \
     FROM transactions t JOIN accounts a ON a.id = t.account_id JOIN ",
    latest_balance!(),
    " WHERE t.pending = 0 AND t.amount_minor < 0 AND t.posted_on > b.on_date \
       AND t.posted_on <= ?1 AND a.include_in_forecast = 1 \
       AND a.liquidity IN ('cash', 'short_term_liability') \
       AND (a.closed_at IS NULL \
            OR (a.liquidity = 'short_term_liability' AND b.balance_minor < 0)) \
     GROUP BY t.currency ORDER BY t.currency"
);

fn count(row: &SqliteRow, column: &str) -> Result<u32, StoreError> {
    u32::try_from(int(row, column)?).map_err(|_| corrupt("a count out of range"))
}

fn category_spent_from_row(row: &SqliteRow) -> Result<CategorySpentRow, StoreError> {
    Ok(CategorySpentRow {
        category: parsed(row, "category")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        spent_minor: int(row, "spent_minor")?,
    })
}

fn loose_ends_from_row(row: &SqliteRow) -> Result<LooseEndsRow, StoreError> {
    Ok(LooseEndsRow {
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        uncategorized_count: count(row, "uncategorized_count")?,
        uncategorized_minor: int(row, "uncategorized_minor")?,
        possible_duplicates: count(row, "possible_duplicates")?,
    })
}

fn newer_spending_from_row(row: &SqliteRow) -> Result<NewerSpendingRow, StoreError> {
    Ok(NewerSpendingRow {
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        total_minor: int(row, "total_minor")?,
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
    /// Backend failures, including a row that no longer parses and a sum too large
    /// for the database.
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
        let goal_spent = sqlx::query(GOAL_SPENT)
            .bind(&day)
            .fetch_all(&mut *tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(|row| Ok((parsed(row, "goal")?, int(row, "spent_minor")?)))
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
            goal_spent,
            category_spent,
            loose_ends,
            newer_spending,
            provider_accounts,
            connections,
        })
    }
}
