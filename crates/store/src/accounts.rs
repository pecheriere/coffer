//! Accounts.

use coffer_domain::accounts::{AccountKind, AccountName, LiquidityClass};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{parsed, text, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, name, kind, currency, liquidity, liquidity_overridden, include_in_net_worth, \
         include_in_forecast, closed_at, created_at"
    };
}

/// A stored account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: AccountId,
    pub name: AccountName,
    pub kind: AccountKind,
    pub currency: Currency,
    pub liquidity: LiquidityClass,
    /// Whether `liquidity` was chosen rather than defaulted from `kind`.
    pub liquidity_overridden: bool,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
    /// When the account was closed; `None` while open.
    pub closed_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// An account to create.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub id: AccountId,
    pub name: AccountName,
    pub kind: AccountKind,
    pub currency: Currency,
    pub liquidity: LiquidityClass,
    pub liquidity_overridden: bool,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
    pub created_at: Timestamp,
}

/// The fields of an account to change; `None` leaves a field as it is. The
/// currency has no field: it never changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountChanges {
    pub name: Option<AccountName>,
    pub kind: Option<AccountKind>,
    pub liquidity: Option<LiquidityClass>,
    pub liquidity_overridden: Option<bool>,
    pub include_in_net_worth: Option<bool>,
    pub include_in_forecast: Option<bool>,
    /// `Some(None)` reopens the account.
    pub closed_at: Option<Option<Timestamp>>,
}

fn flag(row: &SqliteRow, column: &str) -> Result<bool, StoreError> {
    match row
        .try_get::<i64, _>(column)
        .map_err(StoreError::from_sqlx)?
    {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(crate::convert::corrupt("flag is neither 0 nor 1")),
    }
}

fn account_from_row(row: &SqliteRow) -> Result<AccountRecord, StoreError> {
    Ok(AccountRecord {
        id: parsed(row, "id")?,
        name: AccountName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        kind: parsed(row, "kind")?,
        currency: Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?,
        liquidity: parsed(row, "liquidity")?,
        liquidity_overridden: flag(row, "liquidity_overridden")?,
        include_in_net_worth: flag(row, "include_in_net_worth")?,
        include_in_forecast: flag(row, "include_in_forecast")?,
        closed_at: time_opt(row, "closed_at")?,
        created_at: time(row, "created_at")?,
    })
}

async fn fetch_account<'e>(
    executor: impl SqliteExecutor<'e>,
    id: AccountId,
) -> Result<AccountRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM accounts WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    account_from_row(&row)
}

impl StoreTx {
    /// Create an account.
    ///
    /// # Errors
    ///
    /// Backend failures. Callers pass a fresh [`AccountId::new`], so an id
    /// collision is not a practical case.
    pub async fn insert_account(&mut self, new: NewAccount) -> Result<AccountRecord, StoreError> {
        sqlx::query(
            "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
             include_in_net_worth, include_in_forecast, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new.id.to_string())
        .bind(new.name.as_str())
        .bind(new.kind.as_str())
        .bind(new.currency.as_str())
        .bind(new.liquidity.as_str())
        .bind(new.liquidity_overridden)
        .bind(new.include_in_net_worth)
        .bind(new.include_in_forecast)
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(AccountRecord {
            id: new.id,
            name: new.name,
            kind: new.kind,
            currency: new.currency,
            liquidity: new.liquidity,
            liquidity_overridden: new.liquidity_overridden,
            include_in_net_worth: new.include_in_net_worth,
            include_in_forecast: new.include_in_forecast,
            closed_at: None,
            created_at: new.created_at,
        })
    }

    /// Apply `changes` to an account and return it as it now is.
    ///
    /// # Errors
    ///
    /// `NotFound` if the account does not exist.
    pub async fn update_account(
        &mut self,
        id: AccountId,
        changes: AccountChanges,
    ) -> Result<AccountRecord, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE accounts SET id = id");
        if let Some(name) = &changes.name {
            query.push(", name = ").push_bind(name.as_str());
        }
        if let Some(kind) = changes.kind {
            query.push(", kind = ").push_bind(kind.as_str());
        }
        if let Some(liquidity) = changes.liquidity {
            query.push(", liquidity = ").push_bind(liquidity.as_str());
        }
        if let Some(overridden) = changes.liquidity_overridden {
            query
                .push(", liquidity_overridden = ")
                .push_bind(overridden);
        }
        if let Some(include) = changes.include_in_net_worth {
            query.push(", include_in_net_worth = ").push_bind(include);
        }
        if let Some(include) = changes.include_in_forecast {
            query.push(", include_in_forecast = ").push_bind(include);
        }
        if let Some(closed_at) = changes.closed_at {
            query
                .push(", closed_at = ")
                .push_bind(closed_at.map(Timestamp::as_millisecond));
        }
        query.push(" WHERE id = ").push_bind(id.to_string());
        let result = query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())?;
        fetch_account(&mut *self.tx, id).await
    }

    /// Fetch an account, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such account.
    pub async fn get_account(&mut self, id: AccountId) -> Result<AccountRecord, StoreError> {
        fetch_account(&mut *self.tx, id).await
    }
}

impl Store {
    /// Fetch an account.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such account.
    pub async fn get_account(&self, id: AccountId) -> Result<AccountRecord, StoreError> {
        fetch_account(&self.pool, id).await
    }

    /// The accounts within `scope`, oldest first; closed ones only when
    /// `include_closed`.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_accounts(
        &self,
        scope: &AccountScope,
        include_closed: bool,
    ) -> Result<Vec<AccountRecord>, StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(concat!(
            "SELECT ",
            columns!(),
            " FROM accounts WHERE 1 = 1"
        ));
        match scope {
            AccountScope::All => {}
            AccountScope::Only(ids) if ids.is_empty() => return Ok(Vec::new()),
            AccountScope::Only(ids) => {
                query.push(" AND id IN (");
                let mut separated = query.separated(", ");
                for id in ids {
                    separated.push_bind(id.to_string());
                }
                separated.push_unseparated(")");
            }
        }
        if !include_closed {
            query.push(" AND closed_at IS NULL");
        }
        query.push(" ORDER BY id");
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        rows.iter().map(account_from_row).collect()
    }
}
