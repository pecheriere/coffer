//! Store error type. Backend-specific errors are boxed so no database type
//! appears in the public API.

/// The primary `SQLite` result codes that come from the state of the database
/// and not from a statement's values: `BUSY`, `LOCKED`, `NOMEM`, `READONLY`,
/// `IOERR`, `CORRUPT`, `FULL`, `CANTOPEN`, `PROTOCOL` and `NOTADB`.
const DATABASE_FAULT_CODES: [i32; 10] = [5, 6, 7, 8, 10, 11, 13, 14, 15, 26];

/// The text of the injected failure that stands for a database fault, which
/// a test cannot otherwise place on one chosen record.
#[cfg(any(test, feature = "testkit"))]
pub(crate) const INJECTED_FAULT_MESSAGE: &str = "injected database fault";

/// Errors returned by the store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(&'static str),
    /// A value the caller should never pass, refused before it reaches
    /// the database.
    #[error("invalid value: {0}")]
    Invalid(&'static str),
    /// A pagination cursor that this store did not issue or that was altered.
    #[error("invalid pagination cursor")]
    InvalidCursor,
    #[error("storage backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The database itself could not do the work (see
    /// [`StoreError::is_database_fault`]). This says nothing about the data
    /// asked for or written, and the same call may succeed once the database
    /// is well.
    #[error("the database could not do the work")]
    DatabaseFault(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    /// Whether the failure came from the state of the database (busy or
    /// locked, no connection in time, a failing or full disk, a damaged
    /// file) and not from what was asked of it.
    #[must_use]
    pub fn is_database_fault(&self) -> bool {
        matches!(self, Self::DatabaseFault(_))
    }

    pub(crate) fn backend(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(err))
    }

    /// Classify a `sqlx` error: unique violations become [`Self::Conflict`],
    /// a database that cannot do the work [`Self::DatabaseFault`], and
    /// everything else, such as a CHECK violation, an opaque backend failure.
    pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Self::Conflict("unique constraint violated")
            }
            _ if database_fault(&err) => Self::DatabaseFault(Box::new(err)),
            _ => Self::backend(err),
        }
    }
}

/// Whether `err` comes from the state of the database or of the pool, not from
/// what was asked of it.
fn database_fault(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => {
            #[cfg(any(test, feature = "testkit"))]
            if db.message() == INJECTED_FAULT_MESSAGE {
                return true;
            }
            // The code is `SQLite`'s extended one; its low byte is the primary.
            db.code()
                .and_then(|code| code.parse::<i32>().ok())
                .is_some_and(|code| DATABASE_FAULT_CODES.contains(&(code & 0xff)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
    use std::io;
    use std::time::Duration;

    use super::StoreError;

    async fn database(dir: &tempfile::TempDir) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("test.db"))
            .create_if_missing(true)
            .busy_timeout(Duration::ZERO);
        SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_write_lock_held_elsewhere_is_database_fault() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir).await;
        let _held = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();

        let err = pool.begin_with("BEGIN IMMEDIATE").await.unwrap_err();

        assert!(StoreError::from_sqlx(err).is_database_fault());
    }

    #[tokio::test]
    async fn a_full_database_is_database_fault() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir).await;
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA max_page_count = 1")
            .execute(&mut *connection)
            .await
            .unwrap();

        let err = sqlx::query("CREATE TABLE a (b TEXT); CREATE TABLE c (d TEXT)")
            .execute(&mut *connection)
            .await
            .unwrap_err();

        assert!(StoreError::from_sqlx(err).is_database_fault());
    }

    #[test]
    fn no_connection_and_a_failing_disk_are_database_faults() {
        for err in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
            sqlx::Error::Io(io::Error::other("disk")),
        ] {
            assert!(StoreError::from_sqlx(err).is_database_fault());
        }
    }

    /// A file that is no database at all says nothing about any record.
    #[tokio::test]
    async fn a_file_that_is_no_database_is_a_database_fault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.db"), [0xAB_u8; 8192]).unwrap();
        let pool = database(&dir).await;

        let err = sqlx::query("SELECT 1 FROM sqlite_master")
            .fetch_all(&pool)
            .await
            .unwrap_err();

        assert!(StoreError::from_sqlx(err).is_database_fault());
    }

    /// Nor does a page that was damaged after it was written.
    #[tokio::test]
    async fn a_damaged_page_is_a_database_fault() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir).await;
        sqlx::query("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('a')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let path = dir.path().join("test.db");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4096..].fill(0xFF);
        std::fs::write(&path, bytes).unwrap();
        let pool = database(&dir).await;

        let err = sqlx::query("SELECT v FROM t")
            .fetch_all(&pool)
            .await
            .unwrap_err();

        assert!(StoreError::from_sqlx(err).is_database_fault());
    }

    /// What the database refuses because of the value is the record's own
    /// failure: it must keep counting against the record.
    #[tokio::test]
    async fn a_refused_value_is_no_database_fault() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir).await;
        sqlx::query(
            "CREATE TABLE t (v TEXT CHECK (v <> 'no')); \
             CREATE TRIGGER r BEFORE INSERT ON t WHEN NEW.v = 'raised' \
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        for value in ["no", "raised"] {
            let err = sqlx::query("INSERT INTO t (v) VALUES (?)")
                .bind(value)
                .execute(&pool)
                .await
                .unwrap_err();
            assert!(matches!(StoreError::from_sqlx(err), StoreError::Backend(_)));
        }
    }
}
