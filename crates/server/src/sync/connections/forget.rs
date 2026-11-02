//! Forgetting a connection coffer cannot give up at its provider.
//!
//! Removing a connection whose provider bills until told otherwise is
//! refused while coffer cannot reach the provider for it (see
//! [`SyncService::remove_connection`]). When the provider is merely not
//! configured, the sealed token still works and configuring the provider
//! again fixes that, so nothing is forgotten. When the token itself cannot be
//! opened and that is permanent, say because the server key is lost, the
//! member forgets the connection instead: it is marked `removed` and its
//! sealed credentials are erased. coffer can then no longer end the
//! provider's billing, which needs the token.

use std::collections::HashSet;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_store::{ConnectionStatus, ConnectionStatusChange};

use super::{CONNECTION_CHANGED, audit};
use crate::sync::SyncError;
use crate::sync::engine::{CONNECTION_REMOVED, SyncService, ensure_not_removed};

const TOKEN_STILL_OPENS: &str = "this connection's sealed token still opens, so it is not forgotten: \
     configure its provider again if need be and remove it instead";
const CONFIRMATION_MISMATCH: &str =
    "type the connection's label to confirm that it is forgotten without being given up";

impl SyncService {
    /// The connections `actor` may forget now: not removed, their sealed
    /// token cannot be opened, and the actor a passkey session allowed to
    /// remove them.
    ///
    /// # Errors
    ///
    /// Storage failures.
    pub async fn forgettable(&self, actor: &Actor) -> Result<HashSet<ConnectionId>, SyncError> {
        if self
            .authorize_every_account(actor, AccountLevel::Write)
            .await
            .is_err()
        {
            return Ok(HashSet::new());
        }
        Ok(self
            .store
            .list_connections()
            .await?
            .into_iter()
            .filter(|connection| connection.status != ConnectionStatus::Removed)
            .filter(|connection| {
                authorize(
                    actor,
                    Action::ForgetConnection {
                        owner: connection.owner,
                    },
                )
                .is_ok()
            })
            .filter(|connection| self.token_cannot_be_opened(connection))
            .map(|connection| connection.id)
            .collect())
    }

    /// Marks the connection `removed` and erases its sealed credentials
    /// without giving them up at the provider, which may keep billing the
    /// Item. Only for a connection whose sealed token cannot be opened, and
    /// only when `confirmation` is the connection's label. Audited as
    /// `connection.forget` with its reason, never as a removal.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor is a passkey session that may
    /// remove the connection and has write access to every account;
    /// [`SyncError::NotFound`] for an unknown connection;
    /// [`SyncError::Conflict`] for a removed one, one whose token still opens,
    /// or one reconnected meanwhile; [`SyncError::Invalid`] when
    /// `confirmation` is not its label; storage failures.
    pub async fn forget_connection(
        &self,
        actor: &Actor,
        id: ConnectionId,
        confirmation: &str,
    ) -> Result<(), SyncError> {
        authorize(actor, Action::ManageConnections)?;
        let current = self.store.get_connection(id).await?;
        authorize(
            actor,
            Action::ForgetConnection {
                owner: current.owner,
            },
        )?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await?;
        if current.status == ConnectionStatus::Removed {
            return Err(SyncError::Conflict(CONNECTION_REMOVED));
        }
        if !self.token_cannot_be_opened(&current) {
            return Err(SyncError::Conflict(TOKEN_STILL_OPENS));
        }
        if confirmation.trim() != current.label.as_str() {
            return Err(SyncError::Invalid(CONFIRMATION_MISMATCH));
        }

        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let before = ensure_not_removed(&mut tx, id).await?;
        if before.credentials != current.credentials {
            // Reconnected meanwhile: the new credentials may work.
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
            "connection.forget",
            ("connection", id.to_string()),
            AuditChanges::new()
                .change(
                    "status",
                    before.status.as_str(),
                    ConnectionStatus::Removed.as_str(),
                )
                .set("given_up_at_provider", false)
                .set("reason", "sealed token cannot be opened"),
        )
        .await?;
        tx.commit().await?;
        tracing::warn!(
            connection = %id,
            "connection forgotten without being given up: its provider may keep billing it"
        );
        Ok(())
    }
}
