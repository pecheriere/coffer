//! Adding a connection: its first credentials sealed and stored, and link tokens for the providers that need one.

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_domain::sync::SyncTrigger;
use coffer_store::{NewConnection, NewSyncRun};
use secrecy::SecretString;

use super::{ConnectionView, audit, complete_created, requester};
use crate::http::idempotency::IdempotencySlot;
use crate::sync::SyncError;
use crate::sync::engine::{PROVIDER_NOT_OFFERED, SyncService};
use crate::sync::provider::{ConnectInput, ProviderKind};

impl SyncService {
    /// Connects `kind` with `input`, seals the credentials the provider
    /// returns under the new connection's id and stores it `active`,
    /// owned by the actor, with an `initial` run queued. The audit entry
    /// holds the label and provider only. With `slot`, the idempotency key
    /// is completed with the response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account;
    /// [`SyncError::Invalid`] when this server does not offer `kind`;
    /// [`SyncError::PlaidBudgetSpent`] when a public token is sent to Plaid
    /// too often;
    /// [`SyncError::Provider`] when the provider refuses the input;
    /// [`SyncError::Vault`] when the credentials cannot be sealed; storage
    /// failures.
    pub async fn create_connection(
        &self,
        actor: &Actor,
        kind: ProviderKind,
        label: ConnectionLabel,
        input: ConnectInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<ConnectionView, SyncError> {
        self.authorize_connect(actor).await?;
        let provider = self
            .registry
            .get(kind)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        // Only this pair calls Plaid: `SimpleFin` shares this path, agents
        // included, and a provider refuses another's input by itself.
        if matches!(
            (kind, &input),
            (ProviderKind::Plaid, ConnectInput::PlaidPublicToken(_))
        ) {
            self.plaid_budget.spend(actor.member, self.clock.now())?;
        }
        let credentials = provider.connect(input).await.map_err(SyncError::Provider)?;
        let view = match self
            .store_connection(actor, kind, label, &credentials, slot)
            .await
        {
            Ok(view) => view,
            Err(err) => {
                // Nothing stored the credentials: give them up again rather
                // than leave them live at the provider.
                if let Err(disconnect) = provider.disconnect(&credentials).await {
                    tracing::warn!(
                        error = ?disconnect,
                        "could not give up the credentials of a failed connection"
                    );
                }
                return Err(err);
            }
        };
        drop(credentials);
        self.queued.notify_one();
        tracing::info!(connection = %view.id, provider = kind.as_str(), "connection created");
        Ok(view)
    }

    /// Checks that `actor` may add a connection, whose first sync queues
    /// work on accounts not yet granted, so a create can be refused before
    /// its idempotency key is reserved.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account; a failure to resolve access.
    pub async fn authorize_connect(&self, actor: &Actor) -> Result<(), SyncError> {
        authorize(actor, Action::ManageConnections)?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await
    }

    /// Stores a new connection of `kind` with `credentials`, sealed under
    /// its id, and queues its `initial` run (see [`Self::create_connection`]).
    async fn store_connection(
        &self,
        actor: &Actor,
        kind: ProviderKind,
        label: ConnectionLabel,
        credentials: &SecretString,
        slot: Option<IdempotencySlot>,
    ) -> Result<ConnectionView, SyncError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        // Inserted without credentials, then sealed under its id, in the
        // same transaction: no row ever commits without them.
        let created = tx
            .insert_connection(NewConnection {
                provider: kind.as_str(),
                label: label.clone(),
                owner: actor.member,
                credentials: Vec::new(),
                created_at: now,
            })
            .await?;
        let sealed = self
            .vault
            .seal(created.id, credentials)
            .map_err(SyncError::Vault)?;
        tx.set_connection_credentials(created.id, Some(sealed.into()))
            .await?;
        audit(
            &mut tx,
            now,
            actor,
            "connection.create",
            ("connection", created.id.to_string()),
            AuditChanges::new()
                .set("label", label.as_str())
                .set("provider", kind.as_str()),
        )
        .await?;
        tx.insert_sync_run(NewSyncRun {
            connection: created.id,
            trigger: SyncTrigger::Initial,
            requested_by: Some(requester(actor)),
            queued_at: now,
        })
        .await?;
        let record = tx.get_connection(created.id).await?;
        let view = self.connection_view(record, Vec::new());
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// A Plaid link token to set up a new connection for the actor.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account; [`SyncError::Invalid`] when this server does
    /// not offer Plaid; [`SyncError::PlaidBudgetSpent`] when the member asked
    /// Plaid too often; [`SyncError::Provider`] when Plaid refuses; storage
    /// failures.
    pub async fn new_link_token(&self, actor: &Actor) -> Result<SecretString, SyncError> {
        authorize(actor, Action::ManageConnections)?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await?;
        let provider = self
            .registry
            .get(ProviderKind::Plaid)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        self.plaid_budget.spend(actor.member, self.clock.now())?;
        provider
            .link_token(actor.member, None)
            .await
            .map_err(SyncError::Provider)
    }
}
