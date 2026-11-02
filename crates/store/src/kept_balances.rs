//! Balances a provider reported for an account that isn't bound yet, kept so
//! that binding it brings its history: providers only report today's balance,
//! and a snapshot needs an account. Nothing reads them but the bind.

use coffer_domain::accounts::BalanceSource;
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use coffer_domain::money::Currency;
use jiff::Timestamp;
use jiff::civil::Date;

use crate::{StoreError, StoreTx};

/// A balance reported for an unbound provider account on one calendar day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptBalance {
    pub provider_account: ProviderAccountId,
    pub on: Date,
    /// In the minor units of `currency`.
    pub balance_minor: i64,
    /// The currency it was reported in: an account's reported currency can
    /// change while it is unbound, and minor units mean nothing without it.
    pub currency: Currency,
    /// When this value was written.
    pub recorded_at: Timestamp,
}

/// What a bind did with the balances kept for its provider account, for its
/// audit entry. Holds no amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroughtIn {
    /// How many became snapshots.
    pub written: u32,
    /// The earliest day of those, if any.
    pub first_day: Option<Date>,
    /// The latest day of those, if any.
    pub last_day: Option<Date>,
    /// How many were dropped because they are in another currency.
    pub other_currency: u32,
    /// How many were skipped because the account has a snapshot of that day.
    pub already_there: u32,
}

fn count(rows: u64) -> u32 {
    u32::try_from(rows).unwrap_or(u32::MAX)
}

impl StoreTx {
    /// Keep `balance`, replacing what was kept for the same provider account
    /// and day.
    ///
    /// # Errors
    ///
    /// Backend failures, including a provider account that does not exist.
    pub async fn keep_balance(&mut self, balance: KeptBalance) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO kept_balances \
             (provider_account_id, on_date, balance_minor, currency, recorded_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (provider_account_id, on_date) DO UPDATE SET \
             balance_minor = excluded.balance_minor, currency = excluded.currency, \
             recorded_at = excluded.recorded_at",
        )
        .bind(balance.provider_account.to_string())
        .bind(balance.on.to_string())
        .bind(balance.balance_minor)
        .bind(balance.currency.as_str())
        .bind(balance.recorded_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Turn what was kept for `provider_account` in `currency` into `sync`
    /// snapshots of `account`, never over a snapshot already there, and
    /// remove everything kept for it, so nothing can apply twice. What was
    /// kept in another currency has no meaning for the account and is dropped.
    /// The daily rows of the account are rewritten in this transaction, from the
    /// earliest day written on.
    ///
    /// # Errors
    ///
    /// Backend failures, or a day that no longer parses.
    pub async fn bring_in_kept_balances(
        &mut self,
        provider_account: ProviderAccountId,
        account: AccountId,
        currency: Currency,
    ) -> Result<BroughtIn, StoreError> {
        let provider_account = provider_account.to_string();
        let matching: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM kept_balances WHERE provider_account_id = ? AND currency = ?",
        )
        .bind(&provider_account)
        .bind(currency.as_str())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let written: Vec<(String,)> = sqlx::query_as(
            "INSERT INTO balance_snapshots \
             (account_id, on_date, balance_minor, source, recorded_at) \
             SELECT ?, on_date, balance_minor, ?, recorded_at FROM kept_balances \
             WHERE provider_account_id = ? AND currency = ? \
             ON CONFLICT (account_id, on_date) DO NOTHING RETURNING on_date",
        )
        .bind(account.to_string())
        .bind(BalanceSource::Sync.as_str())
        .bind(&provider_account)
        .bind(currency.as_str())
        .fetch_all(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let removed = sqlx::query("DELETE FROM kept_balances WHERE provider_account_id = ?")
            .bind(&provider_account)
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?
            .rows_affected();
        let matching = u64::try_from(matching).map_err(StoreError::backend)?;
        let written_count = u64::try_from(written.len()).map_err(StoreError::backend)?;
        let day = |day: Option<&String>| {
            day.map(|day| day.parse::<Date>().map_err(StoreError::backend))
                .transpose()
        };
        let days = written.iter().map(|(day,)| day);
        // ISO days sort as text.
        let first_day = day(days.clone().min())?;
        if let Some(first_day) = first_day {
            self.refresh_after_bring_in(account, first_day).await?;
        }
        Ok(BroughtIn {
            written: count(written_count),
            first_day,
            last_day: day(days.max())?,
            other_currency: count(removed.saturating_sub(matching)),
            already_there: count(matching.saturating_sub(written_count)),
        })
    }

    /// Remove everything kept for the provider accounts of `connection`.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn discard_kept_balances(
        &mut self,
        connection: ConnectionId,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM kept_balances WHERE provider_account_id IN \
             (SELECT id FROM provider_accounts WHERE connection_id = ?)",
        )
        .bind(connection.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

#[cfg(any(test, feature = "testkit"))]
impl crate::Store {
    /// The days and amounts kept for the provider accounts of `connection`,
    /// oldest first. For tests: production code reads kept balances only by
    /// bringing them in.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn kept_balances_for_tests(
        &self,
        connection: ConnectionId,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        sqlx::query_as(
            "SELECT on_date, balance_minor FROM kept_balances WHERE provider_account_id IN \
             (SELECT id FROM provider_accounts WHERE connection_id = ?) ORDER BY on_date",
        )
        .bind(connection.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)
    }

    /// Make every later `provider_account.bind` audit entry fail to write: the
    /// last write of a bind, for a test that all of it rolls back.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or the trigger cannot be created.
    pub async fn inject_bind_audit_failure_for_tests(&self) -> Result<(), StoreError> {
        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS injected_bind_audit_failure \
             BEFORE INSERT ON audit_events WHEN NEW.action = 'provider_account.bind' \
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END",
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    const INSERT: &str = "INSERT INTO kept_balances \
        (provider_account_id, on_date, balance_minor, currency, recorded_at) \
        VALUES ('feed', ?, 100, 'USD', 0)";

    /// Migration 0017 on a database that holds accounts, provider accounts
    /// and snapshots: all of it stays, and the new table starts empty, with
    /// one row per provider account and day and only well-formed days.
    #[tokio::test]
    async fn migration_keeps_a_populated_database_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(dir.path().join("coffer.db"))
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        for migration in migrator.iter().filter(|migration| migration.version < 17) {
            sqlx::raw_sql(migration.sql.clone())
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::raw_sql(
            "INSERT INTO members (id, username, display_name, role, created_at) \
                 VALUES ('alice', 'alice', '', 'admin', 0); \
             INSERT INTO connections (id, provider, label, owner_id, status, created_at) \
                 VALUES ('bank', 'simplefin', 'Bank', 'alice', 'active', 0); \
             INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
                 include_in_net_worth, include_in_forecast, created_at) \
                 VALUES ('checking', 'Checking', 'checking', 'USD', 'cash', 0, 1, 1, 0); \
             INSERT INTO provider_accounts (id, connection_id, provider_account_id, name, \
                 currency_raw, account_id, first_seen_at, last_seen_at) \
                 VALUES ('feed', 'bank', 'p-1', 'Feed', 'USD', NULL, 0, 0); \
             INSERT INTO balance_snapshots (account_id, on_date, balance_minor, source, recorded_at) \
                 VALUES ('checking', '2026-01-01', 5, 'manual', 0);",
        )
        .execute(&pool)
        .await
        .unwrap();

        for migration in migrator.iter().filter(|migration| migration.version == 17) {
            sqlx::raw_sql(migration.sql.clone())
                .execute(&pool)
                .await
                .unwrap();
        }

        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM balance_snapshots), \
             (SELECT count(*) FROM provider_accounts), (SELECT count(*) FROM kept_balances)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 0));

        sqlx::query(INSERT)
            .bind("2026-01-02")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            sqlx::query(INSERT)
                .bind("2026-01-02")
                .execute(&pool)
                .await
                .is_err(),
            "one row per day"
        );
        assert!(
            sqlx::query(
                "INSERT INTO kept_balances \
                 (provider_account_id, on_date, balance_minor, currency, recorded_at) \
                 VALUES ('nobody', '2026-01-03', 100, 'USD', 0)",
            )
            .execute(&pool)
            .await
            .is_err(),
            "only for a provider account that exists"
        );
        assert!(
            sqlx::query(INSERT)
                .bind("January 3rd")
                .execute(&pool)
                .await
                .is_err(),
            "only a well-formed day"
        );
    }
}
