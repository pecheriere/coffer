//! Store error type. Backend-specific errors are boxed so no database type
//! appears in the public API.

/// The primary `SQLite` result codes that come from the state of the database
/// and not from a statement's values: `BUSY`, `LOCKED`, `NOMEM`, `READONLY`,
/// `IOERR`, `FULL`, `CANTOPEN` and `PROTOCOL`.
const TRANSIENT_SQLITE_CODES: [i32; 8] = [5, 6, 7, 8, 10, 13, 14, 15];

/// The text of the injected failure that stands for a busy database, which
/// a test cannot otherwise place on one chosen record.
#[cfg(any(test, feature = "testkit"))]
pub(crate) const INJECTED_TRANSIENT_MESSAGE: &str = "injected transient failure";

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
    /// The database could not do the work just now (see
    /// [`StoreError::is_transient`]). This says nothing about the data, and
    /// the same call may succeed later.
    #[error("storage temporarily unavailable")]
    Transient(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    /// Whether the failure came from the state of the database (a busy or
    /// locked database, no connection in time, a failing or full disk) and
    /// not from what was asked of it.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    pub(crate) fn backend(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(err))
    }

    /// Classify a `sqlx` error: unique violations become [`Self::Conflict`],
    /// a database that cannot do the work just now [`Self::Transient`], and
    /// everything else, such as a CHECK violation, an opaque backend failure.
    pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Self::Conflict("unique constraint violated")
            }
            _ if comes_and_goes(&err) => Self::Transient(Box::new(err)),
            _ => Self::backend(err),
        }
    }
}

/// Whether `err` comes from the state of the database or of the pool.
fn comes_and_goes(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => {
            #[cfg(any(test, feature = "testkit"))]
            if db.message() == INJECTED_TRANSIENT_MESSAGE {
                return true;
            }
            // The code is `SQLite`'s extended one; its low byte is the primary.
            db.code()
                .and_then(|code| code.parse::<i32>().ok())
                .is_some_and(|code| TRANSIENT_SQLITE_CODES.contains(&(code & 0xff)))
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
    async fn a_write_lock_held_elsewhere_is_transient() {
        let dir = tempfile::tempdir().unwrap();
        let pool = database(&dir).await;
        let _held = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();

        let err = pool.begin_with("BEGIN IMMEDIATE").await.unwrap_err();

        assert!(StoreError::from_sqlx(err).is_transient());
    }

    #[tokio::test]
    async fn a_full_database_is_transient() {
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

        assert!(StoreError::from_sqlx(err).is_transient());
    }

    #[test]
    fn no_connection_and_a_failing_disk_are_transient() {
        for err in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
            sqlx::Error::Io(io::Error::other("disk")),
        ] {
            assert!(StoreError::from_sqlx(err).is_transient());
        }
    }

    /// What the database refuses because of the value stays what it was: a
    /// later attempt would fail the same way.
    #[tokio::test]
    async fn a_refused_value_is_not_transient() {
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
