//! [`AccessResolver`]: the only source of an actor's [`AccountAccess`].
//! Every account-scoped service call obtains access here, so what an agent
//! token's grants allow is decided only by this resolver and the policy.

use coffer_domain::identity::{Actor, Via};
use coffer_domain::policy::{AccountAccess, account_access_for_scope, account_access_with_grants};
use coffer_store::{Store, StoreError};
use jiff::Timestamp;

use super::AuthError;

/// Resolves which accounts an actor may use, and at which level.
#[derive(Debug, Clone)]
pub struct AccessResolver {
    store: Store,
}

impl AccessResolver {
    /// The resolver, reading agent token grants from `store`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// `actor` as it is now, for a long operation that outlives the check its request
    /// passed: a token revoked or expired since, or a member disabled, stops it, and a
    /// member's new role or a token's scope applies. A session is read through its member,
    /// since an `Actor` doesn't name the session.
    ///
    /// # Errors
    ///
    /// [`AuthError::Unauthenticated`] when the actor no longer holds; storage failures.
    pub async fn current(&self, actor: &Actor, now: Timestamp) -> Result<Actor, AuthError> {
        let member = match self.store.get_member(actor.member).await {
            Ok(member) if member.disabled_at.is_none() => member,
            Ok(_) | Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
            Err(other) => return Err(other.into()),
        };
        let via = match actor.via {
            Via::Session => Via::Session,
            Via::Agent { token, .. } => {
                let record = match self.store.get_agent_token(token).await {
                    Ok(record) => record,
                    Err(StoreError::NotFound) => return Err(AuthError::Unauthenticated),
                    Err(other) => return Err(other.into()),
                };
                if record.revoked_at.is_some() || record.expires_at.is_some_and(|at| at <= now) {
                    return Err(AuthError::Unauthenticated);
                }
                Via::Agent {
                    token,
                    scope: record.scope,
                }
            }
        };
        Ok(Actor {
            member: member.id,
            role: member.role,
            via,
        })
    }

    /// The account access of `actor`. A session has uniform access at the
    /// level its member's role allows. An agent token is limited by its
    /// grants, read on every call so a change applies at the next request,
    /// and capped at its effective scope; a token without grants has
    /// uniform access at that scope.
    ///
    /// # Errors
    ///
    /// [`AuthError::Store`] when the grants cannot be read, or one stored
    /// no longer parses: access is then refused, never widened.
    pub fn resolve(
        &self,
        actor: &Actor,
    ) -> impl Future<Output = Result<AccountAccess, AuthError>> + Send + use<> {
        let store = self.store.clone();
        let actor = *actor;
        async move {
            match actor.via {
                Via::Session => Ok(account_access_for_scope(&actor)),
                Via::Agent { token, .. } => {
                    let grants = store.token_grants(token).await?;
                    Ok(account_access_with_grants(&actor, &grants))
                }
            }
        }
    }
}
