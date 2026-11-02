//! Accounts.

use coffer_domain::accounts::{AccountKind, AccountName, BelongsTo, Beneficiary, LiquidityClass};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteExecutor};

use crate::convert::{parsed, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, name, kind, currency, liquidity, liquidity_overridden, include_in_net_worth, \
         include_in_forecast, closed_at, created_at, belongs_to, belongs_to_overridden, beneficiary"
    };
}

/// A stored account.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per 0/1 column of a row: four independent facts, no hidden state machine"
)]
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
    /// Whose money it is.
    pub belongs_to: BelongsTo,
    /// Whether `belongs_to` was chosen rather than defaulted from `kind`.
    pub belongs_to_overridden: bool,
    /// Who or what it is held for; only on an account that isn't the household's.
    pub beneficiary: Option<Beneficiary>,
}

/// An account to create.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per 0/1 column of a row: four independent facts, no hidden state machine"
)]
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
    pub belongs_to: BelongsTo,
    pub belongs_to_overridden: bool,
    pub beneficiary: Option<Beneficiary>,
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
    pub belongs_to: Option<BelongsTo>,
    pub belongs_to_overridden: Option<bool>,
    /// `Some(None)` removes the beneficiary.
    pub beneficiary: Option<Option<Beneficiary>>,
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

pub(crate) fn account_from_row(row: &SqliteRow) -> Result<AccountRecord, StoreError> {
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
        belongs_to: parsed(row, "belongs_to")?,
        belongs_to_overridden: flag(row, "belongs_to_overridden")?,
        beneficiary: text_opt(row, "beneficiary")?
            .map(|label| Beneficiary::parse(&label))
            .transpose()
            .map_err(StoreError::backend)?,
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

/// The accounts in `scope`, ordered by id, which is oldest first: filtered in the
/// query, and with an empty scope never asked.
pub(crate) async fn accounts_in_scope<'e>(
    executor: impl SqliteExecutor<'e>,
    scope: &AccountScope,
    include_closed: bool,
) -> Result<Vec<AccountRecord>, StoreError> {
    let mut query =
        QueryBuilder::<Sqlite>::new(concat!("SELECT ", columns!(), " FROM accounts WHERE 1 = 1"));
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
        .fetch_all(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    rows.iter().map(account_from_row).collect()
}

/// Whether some provider account is bound to `account`.
async fn is_linked<'e>(
    executor: impl SqliteExecutor<'e>,
    account: AccountId,
) -> Result<bool, StoreError> {
    let (linked,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM provider_accounts WHERE account_id = ?)")
            .bind(account.to_string())
            .fetch_one(executor)
            .await
            .map_err(StoreError::from_sqlx)?;
    Ok(linked)
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
             include_in_net_worth, include_in_forecast, created_at, belongs_to, \
             belongs_to_overridden, beneficiary) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(new.belongs_to.as_str())
        .bind(new.belongs_to_overridden)
        .bind(new.beneficiary.as_ref().map(Beneficiary::as_str))
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
            belongs_to: new.belongs_to,
            belongs_to_overridden: new.belongs_to_overridden,
            beneficiary: new.beneficiary,
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
        if let Some(belongs_to) = changes.belongs_to {
            query.push(", belongs_to = ").push_bind(belongs_to.as_str());
        }
        if let Some(overridden) = changes.belongs_to_overridden {
            query
                .push(", belongs_to_overridden = ")
                .push_bind(overridden);
        }
        if let Some(beneficiary) = &changes.beneficiary {
            query
                .push(", beneficiary = ")
                .push_bind(beneficiary.as_ref().map(Beneficiary::as_str));
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

    /// Whether some provider account is bound to `account`, as seen by this
    /// transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn account_is_linked(&mut self, account: AccountId) -> Result<bool, StoreError> {
        is_linked(&mut *self.tx, account).await
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
        accounts_in_scope(&self.pool, scope, include_closed).await
    }

    /// Whether some provider account is bound to `account`.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn account_is_linked(&self, account: AccountId) -> Result<bool, StoreError> {
        is_linked(&self.pool, account).await
    }
}
