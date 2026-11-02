//! Accounts as providers report them, and their bindings to coffer
//! accounts.

use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use jiff::Timestamp;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteExecutor};

use crate::convert::{corrupt, int, parsed, parsed_opt, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, connection_id, provider_account_id, name, institution, currency_raw, kind_hint, \
         account_id, last_balance_raw, last_balance_on, error, first_seen_at, last_seen_at, \
         last_applied_at, held_syncs, hold_released, held_records"
    };
}

/// The statement that inserts a provider account or, when it exists, sets
/// `$on_conflict`.
macro_rules! insert_provider_account {
    ($on_conflict:literal) => {
        concat!(
            "INSERT INTO provider_accounts (id, connection_id, provider_account_id, name, \
             institution, currency_raw, kind_hint, last_balance_raw, last_balance_on, \
             first_seen_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (connection_id, provider_account_id) DO UPDATE SET ",
            $on_conflict,
            " RETURNING id"
        )
    };
}

/// A balance as a provider reported it, unconverted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBalance {
    /// The day it describes.
    pub on: Date,
    /// The amount as a decimal text with all its decimals, in coffer's sign:
    /// a liability's amount owed is negative here even when the provider
    /// reports it as positive.
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
    /// When its data was last applied in full; `None` until then.
    pub last_applied_at: Option<Timestamp>,
    /// How many syncs in a row left the sync cursor in place because this
    /// account's records could not be stored.
    pub held_syncs: u32,
    /// Whether a member asked for the cursor to move past this account's
    /// failing records at the next sync.
    pub hold_released: bool,
    /// How many records the last sync that held the cursor fetched for this
    /// account and could not store.
    pub held_records: u32,
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
        last_applied_at: time_opt(row, "last_applied_at")?,
        held_syncs: u32::try_from(int(row, "held_syncs")?)
            .map_err(|_| corrupt("a negative hold count"))?,
        hold_released: int(row, "hold_released")? != 0,
        held_records: u32::try_from(int(row, "held_records")?)
            .map_err(|_| corrupt("a negative record count"))?,
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
    /// Fetch a provider account inside the transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such provider account.
    pub async fn get_provider_account(
        &mut self,
        id: ProviderAccountId,
    ) -> Result<ProviderAccountRecord, StoreError> {
        fetch_provider_account(&mut *self.tx, id).await
    }

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
        const SQL: &str = insert_provider_account!(
            "name = excluded.name, institution = excluded.institution, \
             currency_raw = excluded.currency_raw, kind_hint = excluded.kind_hint, \
             last_balance_raw = COALESCE(excluded.last_balance_raw, last_balance_raw), \
             last_balance_on = COALESCE(excluded.last_balance_on, last_balance_on), \
             last_seen_at = excluded.last_seen_at"
        );
        self.write_provider_account(SQL, upsert).await
    }

    /// Create a provider account a fetch reported as failed, or note that it
    /// was seen: a failed report says nothing about what the account is, so
    /// an existing account keeps its name, currency, kind and balance.
    ///
    /// # Errors
    ///
    /// Backend failures, including a connection that does not exist.
    pub async fn upsert_failed_provider_account(
        &mut self,
        upsert: ProviderAccountUpsert,
    ) -> Result<ProviderAccountRecord, StoreError> {
        const SQL: &str = insert_provider_account!("last_seen_at = excluded.last_seen_at");
        self.write_provider_account(SQL, upsert).await
    }

    async fn write_provider_account(
        &mut self,
        sql: &'static str,
        upsert: ProviderAccountUpsert,
    ) -> Result<ProviderAccountRecord, StoreError> {
        let seen = upsert.seen_at.as_millisecond();
        let (balance_on, balance_raw) = match upsert.balance {
            Some(balance) => (Some(balance.on.to_string()), Some(balance.amount_raw)),
            None => (None, None),
        };
        let (id,): (String,) = sqlx::query_as(sql)
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

    /// Record that a provider account's data was applied in full at `at`,
    /// which ends any hold of the sync cursor it caused.
    ///
    /// # Errors
    ///
    /// `NotFound` if the provider account does not exist.
    pub async fn set_provider_account_applied(
        &mut self,
        id: ProviderAccountId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE provider_accounts SET last_applied_at = ?, held_syncs = 0, \
             hold_released = 0, held_records = 0 WHERE id = ?",
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Record that a sync left the sync cursor in place because this
    /// provider account's `records` could not be stored.
    ///
    /// # Errors
    ///
    /// `NotFound` if the provider account does not exist.
    pub async fn count_cursor_hold(
        &mut self,
        id: ProviderAccountId,
        records: u32,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE provider_accounts SET held_syncs = held_syncs + 1, held_records = ? \
             WHERE id = ?",
        )
        .bind(i64::from(records))
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Record that the sync cursor no longer waits for this provider
    /// account: its records were passed for good, or fetching them again
    /// cannot help.
    ///
    /// # Errors
    ///
    /// `NotFound` if the provider account does not exist.
    pub async fn clear_cursor_hold(&mut self, id: ProviderAccountId) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE provider_accounts SET held_syncs = 0, hold_released = 0, held_records = 0 \
             WHERE id = ?",
        )
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// End the cursor hold of every provider account of `connection` whose
    /// provider id is not in `present`: a sync that completed without them
    /// says nothing is waiting for them, and an old release must not pass
    /// records they get later.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn clear_cursor_holds_except(
        &mut self,
        connection: ConnectionId,
        present: &[String],
    ) -> Result<(), StoreError> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "UPDATE provider_accounts SET held_syncs = 0, hold_released = 0, held_records = 0 \
             WHERE connection_id = ",
        );
        query.push_bind(connection.to_string());
        query.push(" AND (held_syncs != 0 OR hold_released != 0)");
        if !present.is_empty() {
            query.push(" AND provider_account_id NOT IN (");
            let mut ids = query.separated(", ");
            for id in present {
                ids.push_bind(id.as_str());
            }
            query.push(")");
        }
        query
            .build()
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// The provider accounts of `connection` whose records have held the
    /// sync cursor for at least `min_held_syncs` syncs, by id.
    ///
    /// # Errors
    ///
    /// Backend failures, or an id that no longer parses.
    pub async fn held_provider_accounts(
        &mut self,
        connection: ConnectionId,
        min_held_syncs: u32,
    ) -> Result<Vec<ProviderAccountId>, StoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM provider_accounts WHERE connection_id = ? AND held_syncs >= ? \
             ORDER BY id",
        )
        .bind(connection.to_string())
        .bind(i64::from(min_held_syncs))
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.into_iter()
            .map(|(id,)| id.parse().map_err(StoreError::backend))
            .collect()
    }

    /// Ask that the next sync pass the failing records of the provider
    /// accounts `ids`.
    ///
    /// # Errors
    ///
    /// `NotFound` if one does not exist.
    pub async fn release_cursor_holds(
        &mut self,
        ids: &[ProviderAccountId],
    ) -> Result<(), StoreError> {
        for id in ids {
            let result = sqlx::query("UPDATE provider_accounts SET hold_released = 1 WHERE id = ?")
                .bind(id.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
            expect_one(result.rows_affected())?;
        }
        Ok(())
    }

    /// The provider account of `connection` bound to `account`, if any.
    ///
    /// # Errors
    ///
    /// Backend failures, or an id that no longer parses.
    pub async fn connection_binding(
        &mut self,
        connection: ConnectionId,
        account: AccountId,
    ) -> Result<Option<ProviderAccountId>, StoreError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM provider_accounts WHERE connection_id = ? AND account_id = ? \
             ORDER BY id LIMIT 1",
        )
        .bind(connection.to_string())
        .bind(account.to_string())
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        row.map(|(id,)| id.parse().map_err(StoreError::backend))
            .transpose()
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
    /// How many provider accounts are unbound and belong to a connection
    /// that is not removed: the ones that wait for an account to feed.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn count_unbound_provider_accounts(&self) -> Result<u32, StoreError> {
        let count: i64 = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM provider_accounts p ",
            "JOIN connections c ON c.id = p.connection_id ",
            "WHERE p.account_id IS NULL AND c.status != 'removed'"
        ))
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

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
