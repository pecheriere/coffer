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
    /// remove everything kept for it, so nothing can apply twice. Returns how
    /// many snapshots it wrote. What was kept in another currency has no
    /// meaning for the account and is dropped.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn bring_in_kept_balances(
        &mut self,
        provider_account: ProviderAccountId,
        account: AccountId,
        currency: Currency,
    ) -> Result<u32, StoreError> {
        let provider_account = provider_account.to_string();
        let written = sqlx::query(
            "INSERT INTO balance_snapshots \
             (account_id, on_date, balance_minor, source, recorded_at) \
             SELECT ?, on_date, balance_minor, ?, recorded_at FROM kept_balances \
             WHERE provider_account_id = ? AND currency = ? \
             ON CONFLICT (account_id, on_date) DO NOTHING",
        )
        .bind(account.to_string())
        .bind(BalanceSource::Sync.as_str())
        .bind(&provider_account)
        .bind(currency.as_str())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?
        .rows_affected();
        sqlx::query("DELETE FROM kept_balances WHERE provider_account_id = ?")
            .bind(&provider_account)
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(u32::try_from(written).unwrap_or(u32::MAX))
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
            sqlx::query(INSERT)
                .bind("January 3rd")
                .execute(&pool)
                .await
                .is_err(),
            "only a well-formed day"
        );
    }
}
