//! The ledger: accounts and their balances, categories and transactions.
//! Every operation authorizes through the policy, with account access from
//! the [`AccessResolver`], and writes its audit entry in the same
//! transaction as its change.

pub mod accounts;
pub mod categories;
pub mod transactions;

use std::sync::Arc;

use axum::http::StatusCode;
use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::identity::Actor;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::{AccountLevel, Denied, authorize_account};
use coffer_domain::time::Clock;
use coffer_store::{Store, StoreError, StoreTx};
use jiff::Timestamp;

use crate::auth::{AccessResolver, AuthError, denied_message};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::{Problem, ProblemCode};

/// The ledger service.
pub struct Ledger {
    store: Store,
    clock: Arc<dyn Clock>,
    access: Arc<AccessResolver>,
}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger").finish_non_exhaustive()
    }
}

impl Ledger {
    /// The service over `store`.
    #[must_use]
    pub fn new(store: Store, clock: Arc<dyn Clock>, access: Arc<AccessResolver>) -> Self {
        Self {
            store,
            clock,
            access,
        }
    }

    /// Whether `actor` may use `account` at `level`, decided before a
    /// create reserves its idempotency key. Says nothing of whether the
    /// account exists.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] when the account is hidden or visible below
    /// `level`; a failure to resolve access.
    pub async fn authorize_account_access(
        &self,
        actor: &Actor,
        account: AccountId,
        level: AccountLevel,
    ) -> Result<(), LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, account, level)?;
        Ok(())
    }
}

/// Why a ledger operation failed. Every message is safe to show the
/// requester: none echoes input.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LedgerError {
    /// The actor may not perform the action.
    #[error("{}", denied_message(*.0))]
    Denied(Denied),
    /// No such resource.
    #[error("not found")]
    NotFound,
    /// A submitted value is not acceptable; the message says which.
    #[error("{0}")]
    Invalid(&'static str),
    /// The change conflicts with the current state.
    #[error("{1}")]
    Conflict(ProblemCode, &'static str),
    /// Resolving the actor's access failed.
    #[error(transparent)]
    Auth(AuthError),
    /// A local step (such as serializing a response) failed.
    #[error("internal error")]
    Internal(&'static str),
    /// The database failed.
    #[error("storage error")]
    Store(#[source] StoreError),
    /// Operation `index` of a batch failed with `source`; the batch was not
    /// applied.
    #[error("operations[{index}]: {source}")]
    Operation {
        index: usize,
        #[source]
        source: Box<LedgerError>,
    },
}

impl LedgerError {
    /// `error`, raised by operation `index` of a batch. A storage or
    /// internal failure stays as it is: it is not the operation's fault.
    #[must_use]
    pub fn operation(index: usize, error: Self) -> Self {
        match error {
            Self::Store(_) | Self::Internal(_) | Self::Auth(_) => error,
            other => Self::Operation {
                index,
                source: Box::new(other),
            },
        }
    }
}

impl From<StoreError> for LedgerError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => Self::NotFound,
            other => Self::Store(other),
        }
    }
}

impl From<Denied> for LedgerError {
    fn from(denied: Denied) -> Self {
        Self::Denied(denied)
    }
}

impl From<AuthError> for LedgerError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Denied(denied) => Self::Denied(denied),
            AuthError::NotFound => Self::NotFound,
            AuthError::Store(store) => Self::Store(store),
            other => Self::Auth(other),
        }
    }
}

impl From<LedgerError> for Problem {
    fn from(err: LedgerError) -> Self {
        let message = err.to_string();
        match err {
            LedgerError::Denied(denied) => Self::from(denied),
            LedgerError::NotFound => Self::not_found(),
            LedgerError::Invalid(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::Invalid,
                "Invalid value",
            )
            .with_detail(message),
            LedgerError::Conflict(code, _) => {
                Self::new(StatusCode::CONFLICT, code, "Conflict").with_detail(message)
            }
            LedgerError::Auth(auth) => Self::from(auth),
            // The status and code of the failure, its detail prefixed with
            // the operation's index; never the transaction's id.
            LedgerError::Operation { source, .. } => {
                let mut problem = Self::from(*source);
                problem.detail = Some(message);
                problem
            }
            LedgerError::Store(StoreError::InvalidCursor) => Self::new(
                StatusCode::BAD_REQUEST,
                ProblemCode::InvalidCursor,
                "Invalid pagination cursor",
            ),
            LedgerError::Internal(_) | LedgerError::Store(_) => {
                tracing::error!(error = ?err, "request failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ProblemCode::Internal,
                    "Internal error",
                )
            }
        }
    }
}

/// Appends one audit entry by `actor` inside `tx`, so it commits or rolls
/// back with the change it records.
async fn audit(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    action: &str,
    entity_type: &str,
    entity_id: impl std::fmt::Display,
    changes: AuditChanges,
) -> Result<(), LedgerError> {
    tx.append_audit(
        now,
        &AuditEntry {
            actor: actor.into(),
            action: action.to_owned(),
            entity_type: entity_type.to_owned(),
            entity_id: entity_id.to_string(),
            changes,
        },
    )
    .await?;
    Ok(())
}

/// Completes `slot`, when a key was sent, with the `201 Created` response
/// `view` inside `tx`, so the key, the resource and its audit entry commit
/// together.
async fn complete_created(
    tx: &mut StoreTx,
    slot: Option<IdempotencySlot>,
    view: &impl serde::Serialize,
) -> Result<(), LedgerError> {
    if let Some(slot) = slot {
        let json = serde_json::to_string(view)
            .map_err(|_| LedgerError::Internal("response serialization failed"))?;
        tx.complete_idempotency(slot.member, &slot.key, StatusCode::CREATED.as_u16(), &json)
            .await?;
    }
    Ok(())
}
