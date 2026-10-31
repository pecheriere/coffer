//! Browser sessions and their anti-forgery tokens. Only the keyed hash of a
//! session token is stored; the cookie value itself is never kept or logged.

use std::sync::Arc;

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{MemberId, SessionId};
use coffer_domain::time::Clock;
use coffer_store::{NewSession, Store, StoreError};
use http::HeaderValue;
use jiff::{SignedDuration, Timestamp};
use secrecy::ExposeSecret as _;
use subtle::ConstantTimeEq as _;

use super::AuthError;
use crate::config::Config;
use crate::secrets::{SecretToken, ServerKey, TokenKind, mint};

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "coffer_session";

/// Activity is recorded, and the idle expiry slid, at most this often.
const TOUCH_INTERVAL: SignedDuration = SignedDuration::from_mins(1);

/// Cookie attributes shared by setting and clearing the cookie.
const COOKIE_ATTRIBUTES: &str = "HttpOnly; Secure; SameSite=Strict; Path=/";

/// The session service.
pub struct Sessions {
    store: Store,
    key: Arc<ServerKey>,
    clock: Arc<dyn Clock>,
    config: Config,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions").finish_non_exhaustive()
    }
}

impl Sessions {
    /// The service for `config`'s session lifetimes.
    #[must_use]
    pub fn new(store: Store, key: Arc<ServerKey>, clock: Arc<dyn Clock>, config: Config) -> Self {
        Self {
            store,
            key,
            clock,
            config,
        }
    }

    /// Starts a session for `member` and returns its cookie value, which is
    /// shown only here. Creation is audited by the sign-in that precedes it.
    ///
    /// # Errors
    ///
    /// Storage failures; [`AuthError::Internal`] if a lifetime overflows.
    pub async fn create(&self, member: MemberId) -> Result<SecretToken, AuthError> {
        let now = self.clock.now();
        let absolute_expires_at = add(now, self.config.session_absolute)?;
        let idle_expires_at = add(now, self.config.session_idle)?.min(absolute_expires_at);
        let token = mint(TokenKind::Session);
        let mut tx = self.store.begin().await?;
        tx.insert_session(NewSession {
            token_hash: self.key.hash(TokenKind::Session, token.expose_secret()),
            member,
            created_at: now,
            idle_expires_at,
            absolute_expires_at,
        })
        .await?;
        tx.commit().await?;
        Ok(token)
    }

    /// The actor behind `cookie_value`. The member is read from the store on
    /// every call, so a role change or a disabled member takes effect at the
    /// next request. Activity is recorded at most once a minute, sliding the
    /// idle expiry but never past the absolute one; failing to record it is
    /// logged and does not fail the request.
    ///
    /// # Errors
    ///
    /// [`AuthError::Unauthenticated`] for an unknown or expired session or a
    /// disabled member; storage failures.
    pub async fn resolve(&self, cookie_value: &str) -> Result<Actor, AuthError> {
        let now = self.clock.now();
        let hash = self.key.hash(TokenKind::Session, cookie_value);
        let session = match self.store.find_live_session(&hash, now).await {
            Ok(session) => session,
            Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
            Err(err) => return Err(err.into()),
        };
        let member = match self.store.get_member(session.member).await {
            Ok(member) if member.disabled_at.is_none() => member,
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
            Err(err) => return Err(err.into()),
        };
        if now.duration_since(session.last_seen_at) >= TOUCH_INTERVAL {
            let idle_expires_at =
                add(now, self.config.session_idle)?.min(session.absolute_expires_at);
            tracing::debug!(session = %session.id, "recording session activity");
            match self.touch(session.id, now, idle_expires_at).await {
                // Logged out or disabled meanwhile: nothing to extend.
                Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
                // Recording activity is best effort: the session is valid.
                // It only slides the idle expiry, which the next request
                // retries.
                Err(err) => {
                    tracing::warn!(session = %session.id, error = ?err, "could not record session activity");
                }
                Ok(()) => {}
            }
        }
        Ok(Actor {
            member: member.id,
            role: member.role,
            via: Via::Session,
        })
    }

    async fn touch(
        &self,
        id: SessionId,
        now: Timestamp,
        idle_expires_at: Timestamp,
    ) -> Result<(), StoreError> {
        let mut tx = self.store.begin().await?;
        tx.touch_session(id, now, idle_expires_at).await?;
        tx.commit().await
    }

    /// The anti-forgery token for the session behind `cookie_value`. It is
    /// derived from the session hash, so it is stable for the session's life
    /// and meaningless for any other session. This does not authenticate:
    /// it derives a token for any string, so only hand it to a caller whose
    /// cookie [`Sessions::resolve`] accepted.
    #[must_use]
    pub fn csrf_for(&self, cookie_value: &str) -> String {
        self.key
            .csrf_token(&self.key.hash(TokenKind::Session, cookie_value))
    }

    /// Checks `presented` against the session's anti-forgery token in
    /// constant time. This does not authenticate or check that the session
    /// is live: [`Sessions::resolve`] must have succeeded first.
    ///
    /// # Errors
    ///
    /// [`AuthError::CsrfInvalid`] on any mismatch.
    pub fn check_csrf(&self, cookie_value: &str, presented: &str) -> Result<(), AuthError> {
        let expected = self.csrf_for(cookie_value);
        if bool::from(expected.as_bytes().ct_eq(presented.as_bytes())) {
            Ok(())
        } else {
            Err(AuthError::CsrfInvalid)
        }
    }

    /// Ends the session behind `cookie_value` and audits `session.logout`.
    /// An unknown or already-ended session is not an error.
    ///
    /// # Errors
    ///
    /// Storage failures.
    pub async fn logout(&self, cookie_value: &str) -> Result<(), AuthError> {
        let now = self.clock.now();
        let hash = self.key.hash(TokenKind::Session, cookie_value);
        let session = match self.store.find_live_session(&hash, now).await {
            Ok(session) => session,
            Err(StoreError::NotFound) => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        let mut tx = self.store.begin().await?;
        match tx.delete_session(session.id).await {
            Ok(()) => {}
            Err(StoreError::NotFound) => return Ok(()),
            Err(err) => return Err(err.into()),
        }
        tx.append_audit(
            now,
            &AuditEntry {
                actor: AuditActor::Member {
                    member: session.member,
                    agent: None,
                },
                action: "session.logout".to_owned(),
                entity_type: "member".to_owned(),
                entity_id: session.member.to_string(),
                changes: AuditChanges::new(),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn add(time: Timestamp, by: SignedDuration) -> Result<Timestamp, AuthError> {
    time.checked_add(by)
        .map_err(|_| AuthError::Internal("session lifetime overflow"))
}

/// The `Set-Cookie` value that stores `token`, living `max_age` (the
/// absolute session lifetime). Marked sensitive so HTTP stacks keep it out
/// of logs.
#[must_use]
pub fn session_cookie(token: &SecretToken, max_age: SignedDuration) -> HeaderValue {
    let value = format!(
        "{COOKIE_NAME}={}; {COOKIE_ATTRIBUTES}; Max-Age={}",
        token.expose_secret(),
        max_age.as_secs().max(0),
    );
    // Minted tokens are base64url, always a valid header value; fall back to
    // clearing the cookie rather than ever sending a malformed one.
    let mut header = HeaderValue::from_str(&value).unwrap_or_else(|_| clear_session_cookie());
    header.set_sensitive(true);
    header
}

/// The `Set-Cookie` value that removes the session cookie (logout).
#[must_use]
pub fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "coffer_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0",
    )
}
