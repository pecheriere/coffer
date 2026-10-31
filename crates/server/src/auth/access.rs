//! [`AccessResolver`]: the only source of an actor's [`AccountAccess`].
//! Every account-scoped service call obtains access here, so per-token
//! grants change only this resolver and the policy.

use coffer_domain::identity::Actor;
use coffer_domain::policy::{AccountAccess, account_access_for_scope};

use super::AuthError;

/// Resolves which accounts an actor may use, and at which level.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessResolver;

impl AccessResolver {
    /// The resolver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The account access of `actor`: uniform, at the level its effective
    /// scope allows.
    ///
    /// # Errors
    ///
    /// Uniform access is computed, so it never fails; the `Result` is part
    /// of the contract because access resolved from stored grants can fail
    /// to load.
    pub fn resolve(
        &self,
        actor: &Actor,
    ) -> impl Future<Output = Result<AccountAccess, AuthError>> + Send + use<> {
        std::future::ready(Ok(account_access_for_scope(actor)))
    }
}
