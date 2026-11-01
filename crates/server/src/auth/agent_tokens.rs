//! Agent API tokens: creation, listing, revocation and resolution of a
//! presented bearer token to an actor. Only the keyed hash of a token is
//! stored, and the token itself is shown once, at creation.

use std::sync::Arc;

use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::identity::{Actor, Scope, TokenName, Via};
use coffer_domain::ids::AgentTokenId;
use coffer_domain::policy::{
    Action, GrantTarget, TokenGrant, authorize, check_token_scope, validate_grants,
};
use coffer_domain::time::Clock;
use coffer_store::{AgentTokenRecord, Member, NewAgentToken, Store, StoreError, StoreTx};
use jiff::{SignedDuration, Timestamp};
use secrecy::ExposeSecret as _;

use super::AuthError;
use crate::secrets::{SecretToken, ServerKey, TokenKind, agent_token_prefix, mint};

/// `last_used_at` is recorded at most this often per token.
const TOUCH_INTERVAL: SignedDuration = SignedDuration::from_mins(1);

/// The agent token service.
pub struct AgentTokens {
    store: Store,
    key: Arc<ServerKey>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AgentTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTokens").finish_non_exhaustive()
    }
}

impl AgentTokens {
    /// The service over `store`.
    #[must_use]
    pub fn new(store: Store, key: Arc<ServerKey>, clock: Arc<dyn Clock>) -> Self {
        Self { store, key, clock }
    }

    /// Creates a token for the acting member and returns it with its record.
    /// The secret is returned only here; listings carry just its prefix.
    /// Without `grants` the token has uniform access at its scope; with
    /// them, only the accounts they name, at their levels.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] unless the actor signed in with a passkey
    /// ([`Action::ManageOwnCredentials`]) and `scope` is within the member's
    /// role ceiling; [`AuthError::InvalidGrants`] when `grants` break a
    /// rule of [`validate_grants`]; [`AuthError::UnknownGrantedAccount`]
    /// when one names an account that does not exist; storage failures.
    pub async fn create(
        &self,
        actor: &Actor,
        name: TokenName,
        scope: Scope,
        expires_at: Option<Timestamp>,
        grants: Vec<TokenGrant>,
    ) -> Result<(AgentTokenRecord, SecretToken), AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        check_token_scope(actor.role, scope)?;
        if !grants.is_empty() {
            validate_grants(scope, &grants).map_err(AuthError::InvalidGrants)?;
        }
        let now = self.clock.now();
        let secret = mint(TokenKind::Agent);
        let mut tx = self.store.begin().await?;
        accounts_exist(&mut tx, &grants).await?;
        let record = tx
            .insert_agent_token(NewAgentToken {
                member: actor.member,
                name,
                scope,
                token_hash: self.key.hash(TokenKind::Agent, secret.expose_secret()),
                prefix: agent_token_prefix(secret.expose_secret()),
                created_at: now,
                expires_at,
            })
            .await?;
        tx.replace_token_grants(record.id, &grants).await?;
        tx.append_audit(
            now,
            &AuditEntry {
                actor: actor.into(),
                action: "token.create".to_owned(),
                entity_type: "agent_token".to_owned(),
                entity_id: record.id.to_string(),
                changes: if grants.is_empty() {
                    token_changes(&record)
                } else {
                    token_changes(&record).set("grants", grants_text(&grants))
                },
            },
        )
        .await?;
        tx.commit().await?;
        Ok((record, secret))
    }

    /// Replaces the grants of a token: the actor's own, or anyone's with
    /// [`Action::ManageMembers`]. Only a member signed in with a passkey may
    /// do either; agents never manage tokens. The change applies at the
    /// token's next request. The same grants again record nothing.
    ///
    /// The list can never be emptied: no grants means uniform access, so an
    /// empty list would silently widen a restricted token. To widen one,
    /// grant [`GrantTarget::All`], which the audit log shows.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] unless the actor signed in with a passkey;
    /// [`AuthError::NotFound`] for an unknown token, and also for another
    /// member's token when the actor may not manage members, so existence
    /// is not revealed; [`AuthError::TokenInactive`] for a revoked or
    /// expired one; [`AuthError::InvalidGrants`] when `grants` break a rule
    /// of [`validate_grants`], an empty list included;
    /// [`AuthError::UnknownGrantedAccount`] when one names an account that
    /// does not exist; storage failures.
    pub async fn replace_grants(
        &self,
        actor: &Actor,
        id: AgentTokenId,
        grants: Vec<TokenGrant>,
    ) -> Result<(), AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let record = match tx.get_agent_token(id).await {
            Ok(record)
                if record.member == actor.member
                    || authorize(actor, Action::ManageMembers).is_ok() =>
            {
                record
            }
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::NotFound),
            Err(err) => return Err(err.into()),
        };
        if record.revoked_at.is_some() || record.expires_at.is_some_and(|at| at <= now) {
            return Err(AuthError::TokenInactive);
        }
        validate_grants(record.scope, &grants).map_err(AuthError::InvalidGrants)?;
        accounts_exist(&mut tx, &grants).await?;
        let before = tx.token_grants(id).await?;
        let after_text = grants_text(&grants);
        if grants_text(&before) == after_text {
            return Ok(());
        }
        tx.replace_token_grants(id, &grants).await?;
        tx.append_audit(
            now,
            &AuditEntry {
                actor: actor.into(),
                action: "token.grants".to_owned(),
                entity_type: "agent_token".to_owned(),
                entity_id: id.to_string(),
                changes: if before.is_empty() {
                    AuditChanges::new().set("grants", after_text)
                } else {
                    AuditChanges::new().change("grants", grants_text(&before), after_text)
                },
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The acting member's own tokens, including revoked and expired ones.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] unless the actor signed in with a passkey;
    /// storage failures.
    pub async fn list_own(&self, actor: &Actor) -> Result<Vec<AgentTokenRecord>, AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        Ok(self.store.list_agent_tokens(Some(actor.member)).await?)
    }

    /// Every member's tokens.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] unless the actor signed in with a passkey and
    /// has [`Action::ManageMembers`]; storage failures.
    pub async fn list_all(&self, actor: &Actor) -> Result<Vec<AgentTokenRecord>, AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        authorize(actor, Action::ManageMembers)?;
        Ok(self.store.list_agent_tokens(None).await?)
    }

    /// Revokes a token: the actor's own, or anyone's with
    /// [`Action::ManageMembers`]. Only a member signed in with a passkey may
    /// do either; agents never manage tokens. Revoking a token that is
    /// revoked already succeeds without a second audit entry.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] unless the actor signed in with a passkey;
    /// [`AuthError::NotFound`] for an unknown token, and also for another
    /// member's token when the actor may not manage members, so existence
    /// is not revealed; storage failures.
    pub async fn revoke(&self, actor: &Actor, id: AgentTokenId) -> Result<(), AuthError> {
        authorize(actor, Action::ManageOwnCredentials)?;
        let record = match self.store.get_agent_token(id).await {
            Ok(record)
                if record.member == actor.member
                    || authorize(actor, Action::ManageMembers).is_ok() =>
            {
                record
            }
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::NotFound),
            Err(err) => return Err(err.into()),
        };
        if record.revoked_at.is_some() {
            return Ok(());
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.revoke_agent_token(id, now).await?;
        tx.append_audit(
            now,
            &AuditEntry {
                actor: actor.into(),
                action: "token.revoke".to_owned(),
                entity_type: "agent_token".to_owned(),
                entity_id: id.to_string(),
                changes: token_changes(&record),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The actor behind a presented bearer token. The member is read from
    /// the store on every call, so a role change or a disabled member takes
    /// effect at the next request. `last_used_at` is recorded at most once a
    /// minute.
    ///
    /// # Errors
    ///
    /// [`AuthError::Unauthenticated`] for an unknown, revoked or expired
    /// token or a disabled member; storage failures.
    pub async fn resolve(&self, bearer: &str) -> Result<Actor, AuthError> {
        let now = self.clock.now();
        let hash = self.key.hash(TokenKind::Agent, bearer);
        let token = match self.store.find_live_agent_token(&hash, now).await {
            Ok(token) => token,
            Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
            Err(err) => return Err(err.into()),
        };
        let member = self.active_member(&token).await?;
        let stale = token
            .last_used_at
            .is_none_or(|last| now.duration_since(last) >= TOUCH_INTERVAL);
        if stale {
            tracing::debug!(token = %token.id, "recording token use");
            match self.touch(token.id, now).await {
                // Revoked while this request was in flight.
                Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
                // Usage tracking is best effort: the token is valid.
                Err(err) => {
                    tracing::warn!(token = %token.id, error = ?err, "could not record token use");
                }
                Ok(()) => {}
            }
        }
        Ok(Actor {
            member: member.id,
            role: member.role,
            via: Via::Agent {
                token: token.id,
                scope: token.scope,
            },
        })
    }

    /// The member and token behind an agent actor.
    ///
    /// # Errors
    ///
    /// [`AuthError::Denied`] without [`Action::ReadHousehold`];
    /// [`AuthError::NotFound`] if the actor is not an agent or its token is
    /// gone; storage failures.
    pub async fn describe(&self, actor: &Actor) -> Result<(Member, AgentTokenRecord), AuthError> {
        authorize(actor, Action::ReadHousehold)?;
        let Via::Agent { token, .. } = actor.via else {
            return Err(AuthError::NotFound);
        };
        let record = self.store.get_agent_token(token).await?;
        let member = self.store.get_member(actor.member).await?;
        Ok((member, record))
    }

    async fn touch(&self, id: AgentTokenId, now: Timestamp) -> Result<(), StoreError> {
        let mut tx = self.store.begin().await?;
        tx.touch_agent_token(id, now).await?;
        tx.commit().await
    }

    async fn active_member(&self, token: &AgentTokenRecord) -> Result<Member, AuthError> {
        match self.store.get_member(token.member).await {
            Ok(member) if member.disabled_at.is_none() => Ok(member),
            Ok(_) | Err(StoreError::NotFound) => Err(AuthError::Unauthenticated),
            Err(err) => Err(err.into()),
        }
    }
}

/// Checks that every account `grants` names exists.
async fn accounts_exist(tx: &mut StoreTx, grants: &[TokenGrant]) -> Result<(), AuthError> {
    for grant in grants {
        if let GrantTarget::Account(account) = grant.target {
            match tx.get_account(account).await {
                Ok(_) => {}
                Err(StoreError::NotFound) => return Err(AuthError::UnknownGrantedAccount),
                Err(err) => return Err(err.into()),
            }
        }
    }
    Ok(())
}

/// What a token's audit entries record: who it belongs to and what it may
/// do, never the secret or its hash.
fn token_changes(record: &AgentTokenRecord) -> AuditChanges {
    AuditChanges::new()
        .set("member", record.member)
        .set("name", record.name.as_str())
        .set("scope", record.scope.as_str())
        .set("expires_at", record.expires_at)
}

/// The canonical text of `grants` for the audit log: `all` first, then by
/// account id, each `target=level`, comma-separated
/// (`all=balances,<id>=write`).
fn grants_text(grants: &[TokenGrant]) -> String {
    let mut sorted = grants.to_vec();
    sorted.sort_by_key(|grant| grant.target);
    sorted
        .iter()
        .map(|grant| format!("{}={}", grant.target, grant.level.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}
