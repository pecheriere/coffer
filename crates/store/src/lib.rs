//! SQLite persistence for coffer. The only crate that depends on `sqlx`; its
//! public API speaks domain types only.

mod accounts;
mod agent_tokens;
mod audit;
mod backup;
mod balances;
mod categories;
mod connections;
mod convert;
mod error;
mod household;
mod idempotency;
mod invites;
mod members;
mod passkeys;
mod provider_accounts;
mod sessions;
mod sources;
mod transactions;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub use accounts::{AccountChanges, AccountRecord, NewAccount};
pub use agent_tokens::{AgentTokenRecord, NewAgentToken};
pub use audit::{AuditEvent, AuditFilter};
pub use balances::BalanceSnapshot;
pub use categories::{
    CategoryChanges, CategoryGroupChanges, CategoryGroupRecord, CategoryRecord, NewCategory,
    NewCategoryGroup,
};
pub use connections::{
    ConnectionRecord, ConnectionStatus, ConnectionStatusChange, NewConnection,
    UnknownConnectionStatus,
};
pub use error::StoreError;
pub use idempotency::Reservation;
pub use invites::{Invite, InviteKind, NewInvite};
pub use members::{Member, NewMember};
pub use passkeys::{NewPasskey, PasskeyRecord};
pub use provider_accounts::{ProviderAccountRecord, ProviderAccountUpsert, RawBalance};
pub use sessions::{NewSession, SessionRecord};
pub use sources::{CandidateRow, ImportedFields, NewSource, SourceRecord, SourceUpdate};
pub use transactions::{
    AssigneeFilter, Attribution, NewTransaction, PayeeChange, ReviewChange, SplitRecord,
    TransactionFilter, TransactionRecord, UserFieldChanges,
};

const DEFAULT_PAGE_LIMIT: u32 = 50;
const MAX_PAGE_LIMIT: u32 = 200;

/// Handle to the database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Store {
    pub(crate) pool: SqlitePool,
}

/// An open database transaction. Dropping it without [`StoreTx::commit`]
/// rolls everything back.
#[derive(Debug)]
pub struct StoreTx {
    pub(crate) tx: sqlx::Transaction<'static, sqlx::Sqlite>,
}

impl Store {
    /// Open (creating if missing) the database at `path` and apply pending
    /// migrations.
    ///
    /// # Errors
    ///
    /// Fails if the database cannot be opened or a migration cannot be applied.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            // Temporary tables and indices stay in memory: the container image
            // has no writable temporary directory, and household-scale data
            // keeps them small.
            .pragma("temp_store", "memory");
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .map_err(StoreError::from_sqlx)?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(StoreError::backend)?;
        Ok(Self { pool })
    }

    /// Whether the database answers a trivial query.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or the query fails.
    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Start a write transaction. It takes the write lock up front
    /// (`BEGIN IMMEDIATE`), so a read followed by a write cannot fail with
    /// `SQLITE_BUSY` on a lock upgrade; concurrent writers queue behind the
    /// busy timeout instead.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or the write lock cannot be taken
    /// within the busy timeout.
    pub async fn begin(&self) -> Result<StoreTx, StoreError> {
        let tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(StoreTx { tx })
    }

    /// Run the fixed statement `sql` with one bound text value while CHECK constraints are
    /// ignored, to plant a value the schema would refuse. Tests use it to
    /// prove that such a value cannot break a read. Checks are turned back
    /// on before the connection returns to the pool; if that fails, the
    /// connection is closed instead.
    ///
    /// # Errors
    ///
    /// Fails if no connection is available or a statement fails.
    #[cfg(any(test, feature = "testkit"))]
    pub async fn execute_unchecked_for_tests(
        &self,
        sql: &'static str,
        bind: &str,
    ) -> Result<(), StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut *conn)
            .await
            .map_err(StoreError::from_sqlx)?;
        let result = sqlx::query(sql).bind(bind).execute(&mut *conn).await;
        let restored = sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&mut *conn)
            .await;
        if restored.is_err() {
            conn.close_on_drop();
        }
        result.map_err(StoreError::from_sqlx)?;
        restored.map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

impl StoreTx {
    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Fails if the database rejects the commit.
    pub async fn commit(self) -> Result<(), StoreError> {
        self.tx.commit().await.map_err(StoreError::from_sqlx)
    }
}

/// A request for one page of a listing. `limit` is clamped to `1..=200`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Opaque cursor from a previous [`Page::next`].
    pub after: Option<String>,
    pub limit: u32,
}

impl PageRequest {
    pub(crate) fn effective_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_PAGE_LIMIT)
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            after: None,
            limit: DEFAULT_PAGE_LIMIT,
        }
    }
}

/// One page of results; `next` is `None` on the last page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;

    #[tokio::test]
    async fn corrupt_rows_are_backend_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        let member = coffer_domain::ids::MemberId::new();
        sqlx::query(
            "INSERT INTO members (id, username, display_name, role, created_at) \
             VALUES (?, 'alice', '', 'admin', 0)",
        )
        .bind(member.to_string())
        .execute(&store.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO invites (id, token_hash, kind, created_at, expires_at) \
             VALUES (?, ?, 'reenroll', 0, 10000)",
        )
        .bind(coffer_domain::ids::InviteId::new().to_string())
        .bind([1_u8; 32].as_slice())
        .execute(&store.pool)
        .await
        .unwrap();

        let err = store.get_member(member).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(_)));
        let err = store.list_members().await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(_)));

        let now = Timestamp::from_millisecond(1).unwrap();
        let err = store.find_usable_invite(&[1; 32], now).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(_)));
        let err = store.list_pending_invites(now).await.unwrap_err();
        assert!(matches!(err, StoreError::Backend(_)));
    }

    /// The container image has no writable temporary directory, so SQLite
    /// must never spill temporary tables or indices to disk.
    #[tokio::test]
    async fn temporary_storage_stays_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        let (mode,): (i64,) = sqlx::query_as("PRAGMA temp_store")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(mode, 2, "2 is MEMORY");
    }
}
