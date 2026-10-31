//! Authentication and identity: passkey ceremonies, invites, enrollment,
//! login and member management.

pub mod access;
pub mod agent_tokens;
pub mod identity;
pub mod passkeys;
pub mod sessions;

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::identity::IdentityError;
use coffer_domain::policy::{Denied, DenyReason};
use coffer_store::{StoreError, StoreTx};
use jiff::Timestamp;
use webauthn_rs::prelude::WebauthnError;

pub use access::AccessResolver;
pub use agent_tokens::AgentTokens;
pub use identity::{Identity, InvitePreview};
pub use passkeys::{CeremonyId, Passkeys};
pub use sessions::{COOKIE_NAME, Sessions, clear_session_cookie, session_cookie};

/// Why an identity operation failed. Every message is safe to show the
/// person who attempted it: none echoes input, tokens or internal detail.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The actor may not perform the action.
    #[error("{}", denied_message(*.0))]
    Denied(Denied),
    /// The link is unknown, revoked or expired.
    #[error("this link is invalid or has expired")]
    InviteInvalid,
    /// The link was redeemed already (possibly by a concurrent request).
    #[error("this link has already been used")]
    InviteUsed,
    /// Another member has this username.
    #[error("this username is already taken")]
    UsernameTaken,
    /// Removing the passkey would leave the member unable to sign in.
    #[error("you cannot remove your last passkey")]
    LastPasskey,
    /// The change would leave the household without an active admin.
    #[error("the household must keep at least one active admin")]
    LastAdmin,
    /// Sign-in failed. Deliberately says nothing about why, so an unknown
    /// username and a bad assertion cannot be told apart.
    #[error("sign-in failed")]
    LoginFailed,
    /// The ceremony is unknown, already finished, expired or lost in a
    /// restart.
    #[error("this passkey prompt expired; try again")]
    CeremonyExpired,
    /// Too many ceremonies are in progress.
    #[error("too many passkey prompts in progress; try again in a minute")]
    Busy,
    /// No such member, passkey or pending invite.
    #[error("not found")]
    NotFound,
    /// This credential is registered already (to this or another member).
    #[error("this passkey is already registered")]
    PasskeyAlreadyRegistered,
    /// The member is disabled.
    #[error("this member is disabled")]
    MemberDisabled,
    /// The credential (session cookie or bearer token) is missing, unknown,
    /// expired or belongs to a disabled member. Says nothing about which.
    #[error("authentication required")]
    Unauthenticated,
    /// The anti-forgery token does not match the session.
    #[error("the request could not be verified; reload the page and try again")]
    CsrfInvalid,
    /// A submitted value is invalid.
    #[error(transparent)]
    Invalid(IdentityError),
    /// The authenticator's response did not verify: a client error. A
    /// failed sign-in is reported as [`AuthError::LoginFailed`] instead.
    #[error("the passkey response could not be verified; try again")]
    PasskeyRejected(#[source] WebauthnError),
    /// The relying party configuration is unusable, or `WebAuthn` failed on
    /// the server's side.
    #[error("passkey verification failed")]
    Webauthn(#[source] WebauthnError),
    /// The database failed.
    #[error("storage error")]
    Store(#[source] StoreError),
    /// A local resource (randomness, a lock, time arithmetic) failed.
    #[error("internal error")]
    Internal(&'static str),
}

impl From<StoreError> for AuthError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => Self::NotFound,
            other => Self::Store(other),
        }
    }
}

impl From<Denied> for AuthError {
    fn from(denied: Denied) -> Self {
        Self::Denied(denied)
    }
}

/// What a policy denial tells the requester.
pub(crate) fn denied_message(denied: Denied) -> String {
    match denied.reason {
        DenyReason::SessionRequired => "this action requires signing in with a passkey".to_owned(),
        DenyReason::InsufficientScope { needed } => {
            format!("this action requires {} rights", needed.as_str())
        }
        DenyReason::ScopeAboveRole => "requested scope exceeds your role".to_owned(),
        // Worded like a missing resource, so it reveals nothing even where
        // it is shown (logs, a CLI) instead of the 404 problem.
        DenyReason::AccountHidden => "not found".to_owned(),
        DenyReason::AccountLevel { needed } => {
            format!(
                "this action needs {} access to this account",
                needed.as_str()
            )
        }
    }
}

/// Appends one audit entry inside `tx`, so it commits or rolls back with
/// the change it records.
pub(crate) async fn audit(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &AuditActor,
    action: &str,
    entity_type: &str,
    entity_id: impl std::fmt::Display,
    changes: AuditChanges,
) -> Result<(), AuthError> {
    tx.append_audit(
        now,
        &AuditEntry {
            actor: actor.clone(),
            action: action.to_owned(),
            entity_type: entity_type.to_owned(),
            entity_id: entity_id.to_string(),
            changes,
        },
    )
    .await?;
    Ok(())
}
