//! Accounts as providers report them, and their bindings to coffer
//! accounts.

use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, connection_id, provider_account_id, name, institution, currency_raw, kind_hint, \
         account_id, last_balance_raw, last_balance_on, error, first_seen_at, last_seen_at"
    };
}

/// A balance as a provider gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBalance {
    /// The day it describes.
    pub on: Date,
    /// The amount as a decimal text, unconverted.
    pub amount_raw: String,
}

/// A stored provider account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountRecord {
    pub id: ProviderAccountId,
    pub connection: ConnectionId,
    /// The provider's id for the account.
    pub provider_account_id: String,
    pub name: String,
    pub institution: Option<String>,
    /// The currency as the provider gave it.
    pub currency_raw: String,
    pub kind_hint: Option<AccountKind>,
    /// The coffer account it is bound to; `None` while unbound.
    pub account: Option<AccountId>,
    pub last_balance: Option<RawBalance>,
    /// The provider's last error for this account.
    pub error: Option<String>,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
}

/// A provider account as a fetch reports it: created the first time it is
/// seen, refreshed afterwards. Its binding and error are left as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUpsert {
    pub connection: ConnectionId,
    pub provider_account_id: String,
    pub name: String,
    pub institution: Option<String>,
    pub currency_raw: String,
    pub kind_hint: Option<AccountKind>,
    /// Replaces the last balance when set; `None` keeps it.
    pub balance: Option<RawBalance>,
    pub seen_at: Timestamp,
}

fn provider_account_from_row(row: &SqliteRow) -> Result<ProviderAccountRecord, StoreError> {
    let last_balance = match (
        parsed_opt::<Date>(row, "last_balance_on")?,
        text_opt(row, "last_balance_raw")?,
    ) {
        (Some(on), Some(amount_raw)) => Some(RawBalance { on, amount_raw }),
        (None, None) => None,
        _ => return Err(corrupt("half a balance")),
    };
    Ok(ProviderAccountRecord {
        id: parsed(row, "id")?,
        connection: parsed(row, "connection_id")?,
        provider_account_id: text(row, "provider_account_id")?,
        name: text(row, "name")?,
        institution: text_opt(row, "institution")?,
        currency_raw: text(row, "currency_raw")?,
        kind_hint: parsed_opt(row, "kind_hint")?,
        account: parsed_opt(row, "account_id")?,
        last_balance,
        error: text_opt(row, "error")?,
        first_seen_at: time(row, "first_seen_at")?,
        last_seen_at: time(row, "last_seen_at")?,
    })
}

async fn fetch_provider_account<'e>(
    executor: impl SqliteExecutor<'e>,
    id: ProviderAccountId,
) -> Result<ProviderAccountRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM provider_accounts WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    provider_account_from_row(&row)
}

impl StoreTx {
    /// Create or refresh a provider account, keyed by its connection and
    /// provider id.
    ///
    /// # Errors
    ///
    /// Backend failures, including a connection that does not exist.
    pub async fn upsert_provider_account(
        &mut self,
        upsert: ProviderAccountUpsert,
    ) -> Result<ProviderAccountRecord, StoreError> {
        let seen = upsert.seen_at.as_millisecond();
        let (balance_on, balance_raw) = match upsert.balance {
            Some(balance) => (Some(balance.on.to_string()), Some(balance.amount_raw)),
            None => (None, None),
        };
        let (id,): (String,) = sqlx::query_as(
            "INSERT INTO provider_accounts (id, connection_id, provider_account_id, name, \
             institution, currency_raw, kind_hint, last_balance_raw, last_balance_on, \
             first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (connection_id, provider_account_id) DO UPDATE SET \
             name = excluded.name, institution = excluded.institution, \
             currency_raw = excluded.currency_raw, kind_hint = excluded.kind_hint, \
             last_balance_raw = COALESCE(excluded.last_balance_raw, last_balance_raw), \
             last_balance_on = COALESCE(excluded.last_balance_on, last_balance_on), \
             last_seen_at = excluded.last_seen_at \
             RETURNING id",
        )
        .bind(ProviderAccountId::new().to_string())
        .bind(upsert.connection.to_string())
        .bind(&upsert.provider_account_id)
        .bind(&upsert.name)
        .bind(upsert.institution.as_deref())
        .bind(&upsert.currency_raw)
        .bind(upsert.kind_hint.map(AccountKind::as_str))
        .bind(balance_raw)
        .bind(balance_on)
        .bind(seen)
        .bind(seen)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let id = id.parse().map_err(StoreError::backend)?;
        fetch_provider_account(&mut *self.tx, id).await
    }

    /// Record (or, with `None`, clear) a provider account's error.
    ///
    /// # Errors
    ///
    /// `NotFound` if the provider account does not exist.
    pub async fn set_provider_account_error(
        &mut self,
        id: ProviderAccountId,
        error: Option<String>,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE provider_accounts SET error = ? WHERE id = ?")
            .bind(error)
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Bind an unbound provider account to `account`.
    ///
    /// # Errors
    ///
    /// `NotFound` if the provider account does not exist;
    /// [`StoreError::Conflict`] if it is already bound; backend failures,
    /// including an account that does not exist.
    pub async fn bind_provider_account(
        &mut self,
        id: ProviderAccountId,
        account: AccountId,
    ) -> Result<ProviderAccountRecord, StoreError> {
        let current = fetch_provider_account(&mut *self.tx, id).await?;
        if current.account.is_some() {
            return Err(StoreError::Conflict(
                "the provider account is already bound",
            ));
        }
        sqlx::query("UPDATE provider_accounts SET account_id = ? WHERE id = ?")
            .bind(account.to_string())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        fetch_provider_account(&mut *self.tx, id).await
    }
}

impl Store {
    /// The provider accounts of `connection`, or of every connection, oldest
    /// first.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_provider_accounts(
        &self,
        connection: Option<ConnectionId>,
    ) -> Result<Vec<ProviderAccountRecord>, StoreError> {
        let mut query =
            QueryBuilder::<Sqlite>::new(concat!("SELECT ", columns!(), " FROM provider_accounts"));
        if let Some(connection) = connection {
            query
                .push(" WHERE connection_id = ")
                .push_bind(connection.to_string());
        }
        query.push(" ORDER BY id");
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        rows.iter().map(provider_account_from_row).collect()
    }
}
