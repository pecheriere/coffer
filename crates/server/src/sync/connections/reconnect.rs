//! Reconnecting a connection with new credentials, or confirming a provider-side reauthorization.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_domain::sync::SyncTrigger;
use coffer_store::{
    ConnectionRecord, ConnectionStatus, ConnectionStatusChange, NewSyncRun, StoreTx,
};
use jiff::Timestamp;
use secrecy::SecretString;

use super::requests::check_limits;
use super::{CONNECTION_CHANGED, ConnectionView, ProviderAccountView, audit, requester, visible};
use crate::sync::SyncError;
use crate::sync::engine::{
    CONNECTION_REMOVED, PROVIDER_NOT_OFFERED, SyncService, ensure_not_removed,
};
use crate::sync::provider::{Capabilities, ConnectInput, ProviderKind};
use crate::vault::SealedCredentials;

/// Makes the connection `before` `active` with its error cleared, audits the
/// reconnect and queues a manual run when the limits allow one.
async fn reactivate(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    before: &ConnectionRecord,
    credentials_replaced: bool,
    capabilities: Capabilities,
) -> Result<(), SyncError> {
    let id = before.id;
    tx.update_connection_status(
        id,
        ConnectionStatusChange {
            status: ConnectionStatus::Active,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: now,
        },
    )
    .await?;
    let mut changes = AuditChanges::new().set("credentials_replaced", credentials_replaced);
    if before.status != ConnectionStatus::Active {
        changes = changes.change(
            "status",
            before.status.as_str(),
            ConnectionStatus::Active.as_str(),
        );
    }
    audit(
        tx,
        now,
        actor,
        "connection.reconnect",
        ("connection", id.to_string()),
        changes,
    )
    .await?;
    match check_limits(tx, id, SyncTrigger::Manual, capabilities, now).await {
        Ok(()) => {
            tx.insert_sync_run(NewSyncRun {
                connection: id,
                trigger: SyncTrigger::Manual,
                requested_by: Some(requester(actor)),
                queued_at: now,
            })
            .await?;
            Ok(())
        }
        // A queued run reads the connection when it starts; a running one
        // cannot undo this (the engine updates the status only if the
        // credentials it used are still there and no reauthorization came
        // since). Otherwise the schedule syncs it once the limits allow.
        Err(SyncError::InProgress | SyncError::RateLimited { .. }) => Ok(()),
        Err(err) => Err(err),
    }
}

impl SyncService {
    /// Connects the connection's provider again with `input` and reseals
    /// the new credentials under the same id: the connection is `active`
    /// again with its error cleared, and its provider accounts, bindings
    /// and source records stay valid. A `manual` run is queued when the
    /// connection's limits allow one.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may remove the connection and
    /// has write access to every account;
    /// [`SyncError::NotFound`] for an unknown connection;
    /// [`SyncError::Conflict`] for a removed one; [`SyncError::Invalid`]
    /// when this server does not offer its provider;
    /// [`SyncError::Provider`] when the provider refuses the input, as Plaid
    /// refuses every input (see [`Self::confirm_reauthorized`]);
    /// [`SyncError::Vault`] when the credentials cannot be sealed; storage
    /// failures.
    pub async fn reconnect(
        &self,
        actor: &Actor,
        id: ConnectionId,
        input: ConnectInput,
    ) -> Result<ConnectionView, SyncError> {
        let current = self.reconnectable(actor, id).await?;
        if current.status == ConnectionStatus::Removed {
            return Err(SyncError::Conflict(CONNECTION_REMOVED));
        }
        // Before the provider is asked: claiming a setup token spends it, so
        // nothing after that may turn the reconnect into a failure.
        let access = self.access.resolve(actor).await?;
        let provider = self
            .provider_of(&current)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        let credentials = provider
            .reconnect(input)
            .await
            .map_err(SyncError::Provider)?;
        let record = match self
            .store_reconnect(actor, &current, &credentials, provider.capabilities())
            .await
        {
            Ok(record) => record,
            Err(err) => {
                // Nothing stored the new credentials: give them up again
                // rather than leave them live at the provider.
                if let Err(disconnect) = provider.disconnect(&credentials).await {
                    tracing::warn!(
                        connection = %id,
                        error = ?disconnect,
                        "could not give up the credentials of a failed reconnect"
                    );
                }
                return Err(err);
            }
        };
        drop(credentials);
        self.queued.notify_one();
        tracing::info!(connection = %id, "connection reconnected");
        let accounts = self
            .store
            .list_provider_accounts(Some(id))
            .await?
            .into_iter()
            .filter(|account| visible(&access, account))
            .map(ProviderAccountView::from)
            .collect();
        Ok(self.connection_view(record, accounts))
    }

    /// Checks that `actor` may reconnect the connection `id`, so a form can
    /// be authorized before its input is read.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may remove the connection and
    /// has write access to every account;
    /// [`SyncError::NotFound`] for an unknown connection; storage failures.
    pub async fn authorize_reconnect(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<(), SyncError> {
        self.reconnectable(actor, id).await.map(drop)
    }

    /// The connection `id`, once `actor` is authorized to reconnect it.
    async fn reconnectable(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<ConnectionRecord, SyncError> {
        authorize(actor, Action::ManageConnections)?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await?;
        let current = self.store.get_connection(id).await?;
        authorize(
            actor,
            Action::RemoveConnection {
                owner: current.owner,
            },
        )?;
        Ok(current)
    }

    /// The Plaid connection `id`, once `actor` is authorized to reconnect it.
    /// Any other provider's connection is not found here.
    async fn plaid_reconnectable(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<ConnectionRecord, SyncError> {
        let current = self.reconnectable(actor, id).await?;
        if current.provider != ProviderKind::Plaid.as_str() {
            return Err(SyncError::NotFound);
        }
        Ok(current)
    }

    /// A Plaid link token in update mode for the connection `id`: the member
    /// logs in to the institution again and the access token stays the same.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::confirm_reauthorized`];
    /// [`SyncError::Vault`] when the credentials cannot be opened;
    /// [`SyncError::PlaidBudgetSpent`] when the member asked Plaid too often;
    /// [`SyncError::Provider`] when Plaid refuses.
    pub async fn update_link_token(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<SecretString, SyncError> {
        let current = self.plaid_reconnectable(actor, id).await?;
        let provider = self
            .provider_of(&current)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        let sealed = current
            .credentials
            .ok_or(SyncError::Conflict(CONNECTION_REMOVED))?;
        let credentials = self
            .vault
            .open(id, &SealedCredentials::from(sealed))
            .map_err(SyncError::Vault)?;
        self.plaid_budget.spend(actor.member, self.clock.now())?;
        provider
            .link_token(actor.member, Some(&credentials))
            .await
            .map_err(SyncError::Provider)
    }

    /// Records that the member finished Plaid's update mode. The access
    /// token did not change, so nothing is resealed and the saved cursor
    /// stays valid: the connection is `active` again with its error cleared,
    /// and a `manual` run is queued when its limits allow one, which also
    /// shows whether the login really is back. A run already in flight still
    /// applies its data and saves its cursor, unless the provider reported
    /// errors it can no longer record, but no longer changes the connection's
    /// status; the schedule then syncs it once it is due.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may remove the connection and
    /// has write access to every account; [`SyncError::NotFound`] for an
    /// unknown connection or one of another provider;
    /// [`SyncError::Conflict`] for a removed one; [`SyncError::Invalid`] when
    /// this server does not offer Plaid; storage failures.
    pub async fn confirm_reauthorized(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<(), SyncError> {
        let current = self.plaid_reconnectable(actor, id).await?;
        let provider = self
            .provider_of(&current)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let before = ensure_not_removed(&mut tx, id).await?;
        // The credentials stay, so this is how a run in flight can tell.
        tx.bump_reauth_generation(id).await?;
        reactivate(&mut tx, now, actor, &before, false, provider.capabilities()).await?;
        tx.commit().await?;
        self.queued.notify_one();
        tracing::info!(connection = %id, "connection reauthorized");
        Ok(())
    }

    /// Stores reconnected `credentials` for `current`, sealed under its id,
    /// if it is neither removed nor reconnected since it was read; queues a
    /// manual run when the limits allow one.
    async fn store_reconnect(
        &self,
        actor: &Actor,
        current: &ConnectionRecord,
        credentials: &SecretString,
        capabilities: Capabilities,
    ) -> Result<ConnectionRecord, SyncError> {
        let id = current.id;
        let sealed = self.vault.seal(id, credentials).map_err(SyncError::Vault)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let before = ensure_not_removed(&mut tx, id).await?;
        if before.credentials != current.credentials {
            return Err(SyncError::Conflict(CONNECTION_CHANGED));
        }
        tx.set_connection_credentials(id, Some(sealed.into()))
            .await?;
        // A cursor belongs to the credentials it was given for.
        tx.set_provider_state(id, None).await?;
        reactivate(&mut tx, now, actor, &before, true, capabilities).await?;
        let record = tx.get_connection(id).await?;
        tx.commit().await?;
        Ok(record)
    }
}
