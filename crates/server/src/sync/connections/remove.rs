//! Removing a connection and giving its credentials up at the provider.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_store::{ConnectionRecord, ConnectionStatus, ConnectionStatusChange};

use super::{CONNECTION_CHANGED, audit};
use crate::sync::SyncError;
use crate::sync::engine::{CONNECTION_REMOVED, SyncService, ensure_not_removed};
use crate::sync::provider::{ProviderError, ProviderKind};
use crate::vault::SealedCredentials;

const CANNOT_GIVE_UP: &str = "the provider keeps billing this connection until it is removed there, \
     and coffer cannot reach it: check the server's provider configuration and key, then retry";

impl SyncService {
    /// Gives up the connection's credentials at the provider, then erases
    /// them and marks the connection `removed`. Its provider accounts,
    /// accounts, transactions and bindings stay; no sync writes anything
    /// for it once the removal commits. Credentials that cannot be opened,
    /// a provider this server no longer offers, or a provider that already
    /// refuses them leave nothing to give up, and the removal goes ahead,
    /// unless the provider bills until removed: then it is a conflict.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may remove the connection and
    /// has write access to every account;
    /// [`SyncError::NotFound`] for an unknown connection;
    /// [`SyncError::Conflict`] for a removed one, or one reconnected while
    /// its credentials were given up (retry); [`SyncError::Provider`] when
    /// the provider fails to give up the credentials, with nothing changed;
    /// storage failures.
    pub async fn remove_connection(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<(), SyncError> {
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
        if current.status == ConnectionStatus::Removed {
            return Err(SyncError::Conflict(CONNECTION_REMOVED));
        }
        self.disconnect(&current).await?;

        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let before = ensure_not_removed(&mut tx, id).await?;
        if before.credentials != current.credentials {
            // Reconnected while the old credentials were given up: erasing
            // the new ones would leave them live at the provider.
            return Err(SyncError::Conflict(CONNECTION_CHANGED));
        }
        tx.set_connection_credentials(id, None).await?;
        tx.update_connection_status(
            id,
            ConnectionStatusChange {
                status: ConnectionStatus::Removed,
                last_error: before.last_error,
                attempted_at: None,
                succeeded_at: None,
                at: now,
            },
        )
        .await?;
        audit(
            &mut tx,
            now,
            actor,
            "connection.remove",
            ("connection", id.to_string()),
            AuditChanges::new().change(
                "status",
                before.status.as_str(),
                ConnectionStatus::Removed.as_str(),
            ),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(connection = %id, "connection removed");
        Ok(())
    }

    /// Gives up `connection`'s credentials at its provider, opened only for
    /// the call.
    async fn disconnect(&self, connection: &ConnectionRecord) -> Result<(), SyncError> {
        let Some(sealed) = &connection.credentials else {
            tracing::warn!(
                connection = %connection.id,
                "connection removed without disconnecting: it holds no credentials"
            );
            return Ok(());
        };
        // A provider that bills until told otherwise must be told: its
        // credentials are the only way to, so they are kept until they work.
        let must_give_up = connection
            .provider
            .parse::<ProviderKind>()
            .is_ok_and(ProviderKind::bills_until_removed);
        let Some(provider) = self.provider_of(connection) else {
            if must_give_up {
                return Err(SyncError::Conflict(CANNOT_GIVE_UP));
            }
            tracing::warn!(
                connection = %connection.id,
                "connection removed without disconnecting: its provider is not offered"
            );
            return Ok(());
        };
        let sealed = SealedCredentials::from(sealed.clone());
        let credentials = match self.vault.open(connection.id, &sealed) {
            Ok(credentials) => credentials,
            Err(err) => {
                if must_give_up {
                    tracing::warn!(
                        connection = %connection.id,
                        error = ?err,
                        "connection kept: its credentials cannot be opened"
                    );
                    return Err(SyncError::Conflict(CANNOT_GIVE_UP));
                }
                tracing::warn!(
                    connection = %connection.id,
                    error = ?err,
                    "connection removed without disconnecting: its credentials cannot be opened"
                );
                return Ok(());
            }
        };
        match provider.disconnect(&credentials).await {
            Ok(()) => Ok(()),
            // Credentials the provider refuses are already given up.
            Err(ProviderError::ReauthRequired) => {
                tracing::warn!(
                    connection = %connection.id,
                    "connection removed without disconnecting: the provider refused its credentials"
                );
                Ok(())
            }
            Err(err) => {
                tracing::warn!(connection = %connection.id, error = ?err, "disconnect failed");
                Err(SyncError::Provider(err))
            }
        }
    }
}
