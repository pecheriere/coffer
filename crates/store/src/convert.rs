//! Row decoding helpers shared by the repositories. A stored value that no
//! longer parses is reported as corruption, never a panic.

use std::str::FromStr;

use jiff::Timestamp;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::StoreError;

pub(crate) fn corrupt(what: &'static str) -> StoreError {
    StoreError::backend(std::io::Error::new(std::io::ErrorKind::InvalidData, what))
}

pub(crate) fn timestamp(ms: i64) -> Result<Timestamp, StoreError> {
    Timestamp::from_millisecond(ms).map_err(StoreError::backend)
}

pub(crate) fn text(row: &SqliteRow, column: &str) -> Result<String, StoreError> {
    row.try_get(column).map_err(StoreError::from_sqlx)
}

pub(crate) fn text_opt(row: &SqliteRow, column: &str) -> Result<Option<String>, StoreError> {
    row.try_get(column).map_err(StoreError::from_sqlx)
}

pub(crate) fn blob(row: &SqliteRow, column: &str) -> Result<Vec<u8>, StoreError> {
    row.try_get(column).map_err(StoreError::from_sqlx)
}

pub(crate) fn time(row: &SqliteRow, column: &str) -> Result<Timestamp, StoreError> {
    timestamp(row.try_get(column).map_err(StoreError::from_sqlx)?)
}

pub(crate) fn time_opt(row: &SqliteRow, column: &str) -> Result<Option<Timestamp>, StoreError> {
    row.try_get::<Option<i64>, _>(column)
        .map_err(StoreError::from_sqlx)?
        .map(timestamp)
        .transpose()
}

/// Parse a stored text column with `FromStr` (ids, roles, scopes).
pub(crate) fn parsed<T>(row: &SqliteRow, column: &str) -> Result<T, StoreError>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    text(row, column)?.parse().map_err(StoreError::backend)
}

pub(crate) fn parsed_opt<T>(row: &SqliteRow, column: &str) -> Result<Option<T>, StoreError>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    text_opt(row, column)?
        .map(|s| s.parse())
        .transpose()
        .map_err(StoreError::backend)
}
