//! The provider-agnostic sync engine: what providers report is stored as
//! source records and matched onto canonical transactions (spec §6.2,
//! §8.4).

pub mod apply;
pub mod connections;
pub mod engine;
pub mod provider;
pub mod simplefin;

use axum::http::StatusCode;
use coffer_domain::policy::Denied;
use coffer_store::StoreError;
use jiff::SignedDuration;

use crate::auth::{AuthError, denied_message};
use crate::http::problem::{Problem, ProblemCode};
use crate::ledger::LedgerError;
use crate::vault::VaultError;
use provider::ProviderError;

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
    /// The change conflicts with the current state; the message says how.
    #[error("{0}")]
    Conflict(&'static str),
    /// A ledger operation (creating an account) failed.
    #[error(transparent)]
    Ledger(LedgerError),
    /// The database failed.
    #[error("storage error")]
    Store(#[source] StoreError),
    /// The connection's credentials could not be sealed or opened.
    #[error("the connection's credentials could not be used")]
    Vault(#[source] VaultError),
    /// The data provider failed or refused the request.
    #[error("{0}")]
    Provider(#[source] ProviderError),
    /// Too many syncs of the connection; retry after `retry_after`.
    #[error("too many syncs of this connection")]
    RateLimited { retry_after: SignedDuration },
    /// A sync of the connection is already queued or running.
    #[error("a sync of this connection is already queued or running")]
    InProgress,
    /// The provider account's currency is not the account's, or is not a
    /// supported ISO 4217 currency.
    #[error("the provider account's currency does not match the account's")]
    CurrencyMismatch,
}

/// Whole seconds until `retry_after` has passed, at least one.
pub(crate) fn retry_seconds(retry_after: SignedDuration) -> u32 {
    let mut seconds = retry_after.as_secs();
    if retry_after.subsec_nanos() > 0 {
        seconds = seconds.saturating_add(1);
    }
    u32::try_from(seconds.max(1)).unwrap_or(u32::MAX)
}

impl From<StoreError> for SyncError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => Self::NotFound,
            StoreError::Conflict(message) => Self::Conflict(message),
            other => Self::Store(other),
        }
    }
}

impl From<LedgerError> for SyncError {
    fn from(err: LedgerError) -> Self {
        match err {
            LedgerError::Denied(denied) => Self::Denied(denied),
            LedgerError::NotFound => Self::NotFound,
            LedgerError::Invalid(message) => Self::Invalid(message),
            LedgerError::Store(store) => Self::from(store),
            other => Self::Ledger(other),
        }
    }
}

impl From<AuthError> for SyncError {
    fn from(err: AuthError) -> Self {
        Self::from(LedgerError::from(err))
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
            SyncError::Conflict(_) => {
                Self::new(StatusCode::CONFLICT, ProblemCode::Conflict, "Conflict")
                    .with_detail(message)
            }
            SyncError::Ledger(ledger) => Self::from(ledger),
            SyncError::Provider(ProviderError::ReauthRequired) => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::ReauthRequired,
                "Reconnection required",
            )
            .with_detail(message),
            SyncError::Provider(ref provider) => {
                let detail = provider.record_text();
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    ProblemCode::ProviderError,
                    "Provider error",
                )
                .with_detail(detail)
            }
            SyncError::RateLimited { retry_after } => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ProblemCode::SyncRateLimited,
                "Too many syncs",
            )
            .with_detail(message)
            .with_retry_after(retry_seconds(retry_after)),
            SyncError::InProgress => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::SyncInProgress,
                "Sync in progress",
            )
            .with_detail(message),
            SyncError::CurrencyMismatch => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::CurrencyMismatch,
                "Currency mismatch",
            )
            .with_detail(message),
            SyncError::Store(_) | SyncError::Vault(_) => {
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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use jiff::SignedDuration;

    use super::SyncError;
    use super::provider::ProviderError;
    use crate::http::problem::{Problem, ProblemCode};
    use crate::vault::VaultError;

    #[test]
    fn sync_errors_become_their_problems() {
        let cases = [
            (
                SyncError::Provider(ProviderError::ReauthRequired),
                StatusCode::CONFLICT,
                ProblemCode::ReauthRequired,
            ),
            (
                SyncError::Provider(ProviderError::RateLimited),
                StatusCode::BAD_GATEWAY,
                ProblemCode::ProviderError,
            ),
            (
                SyncError::InProgress,
                StatusCode::CONFLICT,
                ProblemCode::SyncInProgress,
            ),
            (
                SyncError::CurrencyMismatch,
                StatusCode::CONFLICT,
                ProblemCode::CurrencyMismatch,
            ),
            (
                SyncError::Conflict("the provider account is already bound"),
                StatusCode::CONFLICT,
                ProblemCode::Conflict,
            ),
            (
                SyncError::Vault(VaultError::Decrypt),
                StatusCode::INTERNAL_SERVER_ERROR,
                ProblemCode::Internal,
            ),
        ];
        for (err, status, code) in cases {
            let problem = Problem::from(err);
            assert_eq!((problem.status, problem.code), (status, code));
            assert_eq!(problem.retry_after, None);
        }
    }

    #[test]
    fn rate_limit_carries_retry_after_in_whole_seconds() {
        for (retry_after, seconds) in [
            (SignedDuration::from_mins(30), 1800),
            (SignedDuration::from_millis(1500), 2),
            (SignedDuration::ZERO, 1),
        ] {
            let problem = Problem::from(SyncError::RateLimited { retry_after });
            assert_eq!(problem.status, StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(problem.code, ProblemCode::SyncRateLimited);
            assert_eq!(problem.retry_after, Some(seconds));
        }
    }

    /// A provider's text reaches the problem only sanitized.
    #[test]
    fn provider_problem_detail_is_sanitized() {
        let problem = Problem::from(SyncError::Provider(ProviderError::Unavailable(
            "down at https://user:pw@bridge.example/x\u{1b}".into(),
        )));
        assert_eq!(problem.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            problem.detail.as_deref(),
            Some("down at https://***@bridge.example/x")
        );
    }
}
