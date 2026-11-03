//! Leaving a provider account unbound on purpose: dismissing it, and taking that back.
//!
//! A dismissed account stays what an unbound one is: sync keeps its last report and holds its
//! records as before. It only stops raising a forecast caution and stops counting as an account
//! to set up. Binding it later clears the dismissal.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ProviderAccountId;
use coffer_domain::policy::{AccountLevel, Action, authorize, authorize_every_account};

use super::{ProviderAccountView, audit};
use crate::sync::SyncError;
use crate::sync::engine::{SyncService, ensure_not_removed};

impl SyncService {
    /// Dismisses an unbound provider account.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] and write access to every
    /// account; [`SyncError::NotFound`] for an unknown provider account;
    /// [`SyncError::Conflict`] when it is bound, already dismissed, or its connection is
    /// removed; storage failures.
    pub async fn dismiss_provider_account(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
    ) -> Result<ProviderAccountView, SyncError> {
        self.set_dismissed(actor, provider_account, true).await
    }

    /// Takes back a dismissal.
    ///
    /// # Errors
    ///
    /// As [`SyncService::dismiss_provider_account`], with a conflict when it is not dismissed.
    pub async fn restore_provider_account(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
    ) -> Result<ProviderAccountView, SyncError> {
        self.set_dismissed(actor, provider_account, false).await
    }

    async fn set_dismissed(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
        dismiss: bool,
    ) -> Result<ProviderAccountView, SyncError> {
        authorize(actor, Action::ManageConnections)?;
        let access = self.access.resolve(actor).await?;
        authorize_every_account(actor, &access, AccountLevel::Write)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_provider_account(provider_account).await?;
        // No visibility check: passing the every-account check above means the actor sees
        // every provider account, bound or not.
        ensure_not_removed(&mut tx, current.connection).await?;
        let updated = tx
            .set_provider_account_dismissed(provider_account, dismiss.then_some(now))
            .await?;
        audit(
            &mut tx,
            now,
            actor,
            if dismiss {
                "provider_account.dismiss"
            } else {
                "provider_account.restore"
            },
            ("provider_account", provider_account.to_string()),
            AuditChanges::new().set("connection", current.connection.to_string()),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(provider_account = %provider_account, dismissed = dismiss, "provider account dismissal changed");
        Ok(ProviderAccountView::from(updated))
    }
}
