//! [`AccessResolver`]: the only source of an actor's [`AccountAccess`].
//! Every account-scoped service call obtains access here, so what an agent
//! token's grants allow is decided only by this resolver and the policy.

use coffer_domain::identity::{Actor, Via};
use coffer_domain::policy::{AccountAccess, account_access_for_scope, account_access_with_grants};
use coffer_store::Store;

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
