//! What one net worth report reads (spec §8.10), gathered in one read transaction
//! so that a fill or a rebuild can't split it. Everything but the state and the
//! household's settings is filtered in the query by the accounts in scope (N2).

use std::collections::BTreeSet;

use coffer_domain::accounts::BelongsTo;
use coffer_domain::fx::FxCoverage;
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::net_worth::{DailyValue, MIN_VALUE_DATE, NetWorthInterval, period_ends};
use coffer_domain::policy::AccountScope;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row as _, Sqlite, SqliteConnection};

use super::{NetWorthState, fetch_state};
use crate::accounts::accounts_in_scope;
use crate::convert::{corrupt, int, parsed, parsed_opt, text_opt};
use crate::fx::coverage_from_row;
use crate::household::{fetch_display_currency, fetch_forecast_settings};
use crate::{AccountRecord, Store, StoreError};

/// Everything a report for one scope reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetWorthData {
    pub state: NetWorthState,
    pub display: Option<Currency>,
    pub stale_after_days: u16,
    /// The accounts in scope, closed ones too, oldest first.
    pub accounts: Vec<AccountRecord>,
    /// The rows on each period's last day, by account and day, up to the last day filled,
    /// of the accounts a report sums: those included, and every one in a bucket.
    pub rows: Vec<DailyValue>,
    /// The first day each account in scope has a value, of those that have one by
    /// the last day filled. It comes from the snapshots, the source of truth, so
    /// a row missing from the table shows as a hole even on that first day.
    pub first_on: Vec<(AccountId, Date)>,
    /// What is known of fetching the rates of the accounts' currencies and the
    /// display currency, by currency. It names no other currency, but what it says
    /// of these can depend on an account outside the scope: see
    /// [`Store::net_worth_data`].
    pub coverage: Vec<FxCoverage>,
}

fn daily_value_from_row(row: &SqliteRow) -> Result<DailyValue, StoreError> {
    Ok(DailyValue {
        account: parsed(row, "account_id")?,
        on: parsed(row, "on_date")?,
        balance_minor: int(row, "balance_minor")?,
        balance_on: parsed(row, "balance_on")?,
        balance_source: parsed(row, "balance_source")?,
        display: text_opt(row, "display_currency")?
            .map(|code| Currency::parse(&code).map_err(StoreError::backend))
            .transpose()?,
        display_minor: row
            .try_get("display_minor")
            .map_err(StoreError::from_sqlx)?,
        rate_on: parsed_opt(row, "rate_on")?,
        rate_leg: parsed_opt(row, "rate_leg")?,
        conversion: parsed(row, "conversion")?,
    })
}

/// Pushes `(?, ?, …)` for `values`, which must not be empty.
fn push_list<T: ToString>(query: &mut QueryBuilder<Sqlite>, values: &[T]) {
    query.push(" (");
    let mut separated = query.separated(", ");
    for value in values {
        separated.push_bind(value.to_string());
    }
    separated.push_unseparated(")");
}

/// `head` over the rows of `accounts` on `dates`, each named, never a scan.
fn rows_query(head: &'static str, accounts: &[AccountId], dates: &[Date]) -> QueryBuilder<Sqlite> {
    let mut query = QueryBuilder::new(head);
    query.push(" FROM account_daily_values WHERE account_id IN");
    push_list(&mut query, accounts);
    query.push(" AND on_date IN");
    push_list(&mut query, dates);
    query.push(" ORDER BY account_id, on_date");
    query
}

async fn fetch_rows(
    conn: &mut SqliteConnection,
    accounts: &[AccountId],
    dates: &[Date],
    display: Option<Currency>,
) -> Result<Vec<DailyValue>, StoreError> {
    if accounts.is_empty() || dates.is_empty() {
        return Ok(Vec::new());
    }
    let rows = rows_query("SELECT *", accounts, dates)
        .build()
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(daily_value_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    if rows.iter().any(|row| row.display != display) {
        return Err(corrupt("a daily value is not in the display currency"));
    }
    Ok(rows)
}

async fn fetch_first_on(
    conn: &mut SqliteConnection,
    accounts: &[AccountId],
    through: Date,
) -> Result<Vec<(AccountId, Date)>, StoreError> {
    if accounts.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT account_id, MIN(on_date) AS first_on FROM balance_snapshots WHERE account_id IN",
    );
    push_list(&mut query, accounts);
    query.push(" GROUP BY account_id ORDER BY account_id");
    let rows = query
        .build()
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let mut first_on = Vec::new();
    for row in &rows {
        let first = parsed::<Date>(row, "first_on")?.max(MIN_VALUE_DATE);
        if first <= through {
            first_on.push((parsed(row, "account_id")?, first));
        }
    }
    Ok(first_on)
}

async fn fetch_coverage(
    conn: &mut SqliteConnection,
    accounts: &[AccountRecord],
    display: Option<Currency>,
) -> Result<Vec<FxCoverage>, StoreError> {
    let codes: BTreeSet<Currency> = accounts
        .iter()
        .map(|one| one.currency)
        .chain(display)
        .collect();
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let codes: Vec<Currency> = codes.into_iter().collect();
    let mut query = QueryBuilder::<Sqlite>::new("SELECT * FROM fx_coverage WHERE currency IN");
    push_list(&mut query, &codes);
    query.push(" ORDER BY currency");
    query
        .build()
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(coverage_from_row)
        .collect()
}

impl Store {
    /// The daily table's state.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn net_worth_state(&self) -> Result<NetWorthState, StoreError> {
        fetch_state(&self.pool).await
    }

    /// Everything a report over `[from, to]` for `scope` reads, seen at one moment.
    /// Rows stop at the last day filled, and while a rebuild is needed there are
    /// none. `accounts`, `rows` and `first_on` hold only what `scope` covers, and
    /// `state` and the settings are the household's.
    ///
    /// `coverage` is limited to the currencies of `accounts` and the display currency,
    /// yet it can still depend on an account outside `scope`: a currency is fetched
    /// only when the household needs rates at all (N7), so a hidden account in another
    /// currency can be why a visible currency has an entry, and what it says.
    /// **A caller must not expose `coverage` raw:** it only explains why a rate that a
    /// visible row lacks is missing.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for a range past the most points a report answers. That
    /// is checked only here, after the state is read, on `[from, min(to, last day
    /// filled)]`, and not at all while a rebuild is needed or when nothing of the
    /// range is filled. So whether it refuses depends on the table, and a caller whose
    /// refusal must depend on the query alone checks `period_ends` itself first.
    ///
    /// Backend failures, including a row in another display currency than the
    /// setting's, which is corruption: never a number in the wrong units.
    pub async fn net_worth_data(
        &self,
        scope: &AccountScope,
        from: Date,
        to: Date,
        interval: NetWorthInterval,
    ) -> Result<NetWorthData, StoreError> {
        // One read transaction, the state first: a fill or a rebuild can't split it.
        let mut tx = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let state = fetch_state(&mut *tx).await?;
        let display = fetch_display_currency(&mut *tx).await?;
        let stale_after_days = fetch_forecast_settings(&mut *tx).await?.stale_after_days;
        let accounts = accounts_in_scope(&mut *tx, scope, true).await?;
        let (rows, first_on) = match state.current_through() {
            Some(through) => {
                let included: Vec<AccountId> = accounts
                    .iter()
                    .filter(|one| {
                        one.include_in_net_worth || one.belongs_to != BelongsTo::Household
                    })
                    .map(|one| one.id)
                    .collect();
                let every: Vec<AccountId> = accounts.iter().map(|one| one.id).collect();
                let end = to.min(through);
                let dates: Vec<Date> = if from <= end {
                    period_ends(from, end, interval)
                        .map_err(|_| StoreError::Invalid("a range a report refuses"))?
                        .iter()
                        .map(|period| period.on)
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    fetch_rows(&mut tx, &included, &dates, display).await?,
                    fetch_first_on(&mut tx, &every, through).await?,
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let coverage = fetch_coverage(&mut tx, &accounts, display).await?;
        tx.commit().await.map_err(StoreError::from_sqlx)?;
        Ok(NetWorthData {
            state,
            display,
            stale_after_days,
            accounts,
            rows,
            first_on,
            coverage,
        })
    }
}

#[cfg(any(test, feature = "testkit"))]
impl Store {
    /// Every row of the table, by account and day. For tests: a read in production
    /// always names its accounts and dates.
    ///
    /// # Errors
    ///
    /// Backend failures, including a row that no longer parses.
    pub async fn daily_values_for_tests(&self) -> Result<Vec<DailyValue>, StoreError> {
        sqlx::query("SELECT * FROM account_daily_values ORDER BY account_id, on_date")
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(daily_value_from_row)
            .collect()
    }

    /// How `SQLite` runs the rows query a report uses, step by step.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn explain_net_worth_rows_for_tests(
        &self,
        accounts: &[AccountId],
        dates: &[Date],
    ) -> Result<Vec<String>, StoreError> {
        rows_query("EXPLAIN QUERY PLAN SELECT *", accounts, dates)
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?
            .iter()
            .map(|row| crate::convert::text(row, "detail"))
            .collect()
    }
}
