//! The provider-agnostic sync engine: what providers report is stored as
//! source records and matched onto canonical transactions (spec §6.2,
//! §8.4).

pub mod apply;

use axum::http::StatusCode;
use coffer_domain::policy::Denied;
use coffer_store::StoreError;

use crate::auth::denied_message;
use crate::http::problem::{Problem, ProblemCode};

/// Why a sync operation failed. Every message is safe to show the
/// requester: none echoes provider data or input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The actor may not perform the action.
    #[error("{}", denied_message(*.0))]
    Denied(Denied),
    /// No such resource.
    #[error("not found")]
    NotFound,
    /// A value is not acceptable; the message says which.
    #[error("{0}")]
    Invalid(&'static str),
    /// The database failed.
    #[error("storage error")]
    Store(#[source] StoreError),
}

impl From<StoreError> for SyncError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => Self::NotFound,
            other => Self::Store(other),
        }
    }
}

impl From<Denied> for SyncError {
    fn from(denied: Denied) -> Self {
        Self::Denied(denied)
    }
}

impl From<SyncError> for Problem {
    fn from(err: SyncError) -> Self {
        let message = err.to_string();
        match err {
            SyncError::Denied(denied) => Self::from(denied),
            SyncError::NotFound => Self::not_found(),
            SyncError::Invalid(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::Invalid,
                "Invalid value",
            )
            .with_detail(message),
            SyncError::Store(_) => {
                tracing::error!(error = ?err, "sync failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ProblemCode::Internal,
                    "Internal error",
                )
            }
        }
    }
}
