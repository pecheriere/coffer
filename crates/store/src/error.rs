//! Store error type. Backend-specific errors are boxed so no database type
//! appears in the public API.

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
}

impl StoreError {
    pub(crate) fn backend(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(err))
    }

    /// Classify a `sqlx` error: unique violations become [`Self::Conflict`],
    /// everything else is an opaque backend failure.
    pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Self::Conflict("unique constraint violated")
            }
            _ => Self::backend(err),
        }
    }
}
