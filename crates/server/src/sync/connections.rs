//! Connections to data providers and the accounts they report (spec §6.2,
//! §6.11): adding, reconnecting and removing a connection, requesting a
//! sync within the provider's limits, and binding provider accounts to
//! coffer accounts.
//!
//! Credentials exist in clear only in memory: setup input goes to the
//! provider, the credentials it returns are sealed under the connection's
//! id before anything is stored, and they are opened again only to give
//! them up at the provider when the connection is removed. No audit entry,
//! log line, error or view ever holds them.

use coffer_domain::accounts::{AccountKind, ConnectionLabel, LiquidityClass};
use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, MemberId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{
    AccountAccess, AccountLevel, Action, authorize, authorize_account, authorize_every_account,
};
use coffer_domain::sync::SyncTrigger;
use coffer_store::{
    ConnectionRecord, ConnectionStatus, ConnectionStatusChange, NewConnection, NewSyncRun, Page,
    PageRequest, ProviderAccountRecord, StoreTx,
};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use secrecy::SecretString;
use serde::Serialize;
use utoipa::ToSchema;

use super::SyncError;
use super::apply::apply_held;
use super::engine::{
    CONNECTION_REMOVED, PROVIDER_NOT_OFFERED, SyncRunView, SyncService, ensure_not_removed,
};
use super::provider::{Capabilities, ConnectInput, ProviderError, ProviderKind};
use crate::http::idempotency::IdempotencySlot;
use crate::ledger::accounts::{AccountView, NewAccountInput, ValidNewAccount, parse_name};
use crate::ledger::{Ledger, LedgerError};
use crate::vault::SealedCredentials;

const ALREADY_BOUND: &str = "the provider account is already bound";
const ACCOUNT_CLOSED: &str = "the account is closed";
const NEEDS_RECONNECT: &str = "the connection needs reconnecting";
const ACCOUNT_ALREADY_FED: &str =
    "another provider account of this connection is already bound to the account";
const KIND_REQUIRED: &str = "kind is required: the provider gives no kind for this account";
const TRIGGER_INVALID: &str =
    "a member or agent requests manual syncs; only the schedule requests scheduled ones";
const SERIALIZATION_FAILED: &str = "response serialization failed";
const NAME_REQUIRED: &str = "name is required: the provider gives no usable name for this account";
const CONNECTION_CHANGED: &str =
    "the connection was reconnected or removed meanwhile; retry the request";

/// The window the daily budget counts runs in.
const BUDGET_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// The longest account name, in characters.
const MAX_ACCOUNT_NAME_CHARS: usize = 100;

/// Where a connection stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Syncing normally.
    Active,
    /// The last sync failed; the schedule keeps trying.
    Error,
    /// The provider refuses the credentials: reconnect it.
    ReauthRequired,
    /// Removed; its accounts and transactions stay.
    Removed,
}

impl From<ConnectionStatus> for ConnectionState {
    fn from(status: ConnectionStatus) -> Self {
        match status {
            ConnectionStatus::Active => Self::Active,
            ConnectionStatus::Error => Self::Error,
            ConnectionStatus::ReauthRequired => Self::ReauthRequired,
            ConnectionStatus::Removed => Self::Removed,
        }
    }
}

/// A balance as the provider reported it, unconverted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RawBalanceView {
    /// The calendar day it describes (ISO 8601).
    pub on: Date,
    /// The amount as the provider's decimal text.
    pub amount_raw: String,
}

/// An account as a provider reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ProviderAccountView {
    pub id: ProviderAccountId,
    pub connection: ConnectionId,
    pub name: String,
    #[schema(required)]
    pub institution: Option<String>,
    /// The currency as the provider gave it.
    pub currency: String,
    /// The kind the provider suggests, if any.
    #[schema(required)]
    pub kind_hint: Option<AccountKind>,
    /// The coffer account it is bound to; `null` while unbound.
    #[schema(required)]
    pub account: Option<AccountId>,
    /// The last balance the provider reported.
    #[schema(required)]
    pub balance: Option<RawBalanceView>,
    /// The provider's last error for this account, sanitized.
    #[schema(required)]
    pub error: Option<String>,
    pub last_seen_at: Timestamp,
}

impl From<ProviderAccountRecord> for ProviderAccountView {
    fn from(record: ProviderAccountRecord) -> Self {
        Self {
            id: record.id,
            connection: record.connection,
            name: record.name,
            institution: record.institution,
            currency: record.currency_raw,
            kind_hint: record.kind_hint,
            account: record.account,
            balance: record.last_balance.map(|balance| RawBalanceView {
                on: balance.on,
                amount_raw: balance.amount_raw,
            }),
            error: record.error,
            last_seen_at: record.last_seen_at,
        }
    }
}

/// A connection to a data provider. Never shows its credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConnectionView {
    pub id: ConnectionId,
    /// The provider's name, such as `simplefin`.
    pub provider: String,
    pub label: String,
    /// The member who added it.
    pub owner: MemberId,
    pub status: ConnectionState,
    /// The last error, sanitized; `null` after a clean sync.
    #[schema(required)]
    pub last_error: Option<String>,
    #[schema(required)]
    pub last_attempt_at: Option<Timestamp>,
    #[schema(required)]
    pub last_success_at: Option<Timestamp>,
    /// When the schedule syncs it next: the last attempt plus the longer of
    /// the server's sync interval and the provider's minimum. `null` before
    /// the first attempt and for connections the schedule skips.
    #[schema(required)]
    pub next_sync_at: Option<Timestamp>,
    /// Its provider accounts the actor may see.
    pub accounts: Vec<ProviderAccountView>,
}

/// An account to create for a provider account and bind to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLinkedAccountInput {
    /// By default the provider account's name.
    pub name: Option<String>,
    /// By default the provider's kind hint; required without one.
    pub kind: Option<AccountKind>,
    /// By default the kind's class.
    pub liquidity: Option<LiquidityClass>,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
}

/// An account created for a provider account, and the provider account
/// bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LinkedAccountView {
    pub account: AccountView,
    pub provider_account: ProviderAccountView,
}

/// Whether an actor holding `access` may see `provider_account`: a bound
/// one follows its account (visible at any level); an unbound one could be
/// bound to any account, so only an actor seeing every account's
/// transactions sees it.
fn visible(access: &AccountAccess, provider_account: &ProviderAccountRecord) -> bool {
    match provider_account.account {
        Some(account) => access.level_for(account).is_some(),
        None => access.sees_every_account(AccountLevel::Transactions),
    }
}

/// Who requested a run: the member and the agent token they used.
fn requester(actor: &Actor) -> (MemberId, Option<AgentTokenId>) {
    let agent = match actor.via {
        Via::Agent { token, .. } => Some(token),
        Via::Session => None,
    };
    (actor.member, agent)
}

/// Appends one audit entry by `actor` about `entity_type` `entity_id`
/// inside `tx`, so it commits or rolls back with its change.
async fn audit(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    action: &str,
    entity: (&str, String),
    changes: AuditChanges,
) -> Result<(), SyncError> {
    let (entity_type, entity_id) = entity;
    tx.append_audit(
        now,
        &AuditEntry {
            actor: actor.into(),
            action: action.to_owned(),
            entity_type: entity_type.to_owned(),
            entity_id,
            changes,
        },
    )
    .await?;
    Ok(())
}

/// Refuses a run that the connection's limits do not allow now:
/// [`SyncError::InProgress`] while a run is queued or running, else
/// [`SyncError::RateLimited`] with the wait until both the minimum interval
/// since the last start (`min_scheduled_interval` for the schedule,
/// `min_interval` otherwise) and the daily budget allow one.
async fn check_limits(
    tx: &mut StoreTx,
    connection: ConnectionId,
    trigger: SyncTrigger,
    capabilities: Capabilities,
    now: Timestamp,
) -> Result<(), SyncError> {
    let since = now.checked_sub(BUDGET_WINDOW).unwrap_or(Timestamp::MIN);
    let counts = tx.sync_run_counts(connection, since).await?;
    if counts.in_flight {
        return Err(SyncError::InProgress);
    }
    let interval = match trigger {
        SyncTrigger::Schedule => capabilities.min_scheduled_interval,
        SyncTrigger::Manual | SyncTrigger::Initial => capabilities.min_interval,
    };
    let mut wait = SignedDuration::ZERO;
    if let Some(last) = counts.last_started_at {
        let ready = last.checked_add(interval).unwrap_or(Timestamp::MAX);
        wait = wait.max(ready.duration_since(now));
    }
    if counts.started_or_pending >= capabilities.daily_budget {
        // Nothing is in flight, so every counted run started in the window:
        // a run is allowed once enough of them have left it.
        let skip = counts.started_or_pending - capabilities.daily_budget;
        let frees_at = tx
            .sync_run_start_since(connection, since, skip)
            .await?
            .unwrap_or(now)
            .checked_add(BUDGET_WINDOW)
            .unwrap_or(Timestamp::MAX);
        wait = wait.max(frees_at.duration_since(now));
    }
    if wait.is_positive() {
        return Err(SyncError::RateLimited { retry_after: wait });
    }
    Ok(())
}

/// Completes `slot`, when a key was sent, with the `201 Created` response
/// `view` inside `tx`.
async fn complete_created(
    tx: &mut StoreTx,
    slot: Option<IdempotencySlot>,
    view: &impl Serialize,
) -> Result<(), SyncError> {
    if let Some(slot) = slot {
        let json = serde_json::to_string(view)
            .map_err(|_| SyncError::Ledger(LedgerError::Internal(SERIALIZATION_FAILED)))?;
        tx.complete_idempotency(slot.owner, &slot.key, 201, &json)
            .await?;
    }
    Ok(())
}

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
        // A queued run uses the current credentials when it starts; a
        // running one cannot undo a reconnect that replaced them (the engine
        // updates the status only for the credentials it used). Otherwise
        // the schedule syncs it once the limits allow.
        Err(SyncError::InProgress | SyncError::RateLimited { .. }) => Ok(()),
        Err(err) => Err(err),
    }
}

/// The default name of an account created for a provider account: its
/// name, cut to the longest account name.
fn default_account_name(provider_account: &ProviderAccountRecord) -> String {
    provider_account
        .name
        .chars()
        .take(MAX_ACCOUNT_NAME_CHARS)
        .collect()
}

impl SyncService {
    /// The provider a connection uses, if this server offers it.
    pub(super) fn provider_of(
        &self,
        connection: &ConnectionRecord,
    ) -> Option<std::sync::Arc<dyn super::provider::Provider>> {
        connection
            .provider
            .parse::<ProviderKind>()
            .ok()
            .and_then(|kind| self.registry.get(kind))
    }

    /// How long the schedule waits after an attempt to sync `connection`
    /// again: the longer of the server's sync interval and its provider's
    /// minimum between scheduled syncs.
    pub(super) fn effective_interval(&self, connection: &ConnectionRecord) -> SignedDuration {
        self.provider_of(connection)
            .map_or(self.config.sync_interval, |provider| {
                provider
                    .capabilities()
                    .min_scheduled_interval
                    .max(self.config.sync_interval)
            })
    }

    fn connection_view(
        &self,
        connection: ConnectionRecord,
        accounts: Vec<ProviderAccountView>,
    ) -> ConnectionView {
        let next_sync_at = match connection.status {
            ConnectionStatus::Active | ConnectionStatus::Error => {
                let interval = self.effective_interval(&connection);
                connection
                    .last_attempt_at
                    .map(|at| at.checked_add(interval).unwrap_or(Timestamp::MAX))
            }
            ConnectionStatus::ReauthRequired | ConnectionStatus::Removed => None,
        };
        ConnectionView {
            id: connection.id,
            provider: connection.provider,
            label: connection.label.as_str().to_owned(),
            owner: connection.owner,
            status: connection.status.into(),
            last_error: connection.last_error,
            last_attempt_at: connection.last_attempt_at,
            last_success_at: connection.last_success_at,
            next_sync_at,
            accounts,
        }
    }

    /// Checks that `actor` may perform an action needing `level` on every
    /// account (the policy's rule table).
    async fn authorize_every_account(
        &self,
        actor: &Actor,
        level: AccountLevel,
    ) -> Result<(), SyncError> {
        let access = self.access.resolve(actor).await?;
        authorize_every_account(actor, &access, level)?;
        Ok(())
    }

    /// Every connection, removed ones included, oldest first, each with the
    /// provider accounts the actor may see.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ReadHousehold`]; storage
    /// failures.
    pub async fn list_connections(&self, actor: &Actor) -> Result<Vec<ConnectionView>, SyncError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let mut accounts = self.store.list_provider_accounts(None).await?;
        accounts.retain(|account| visible(&access, account));
        let connections = self.store.list_connections().await?;
        Ok(connections
            .into_iter()
            .map(|connection| {
                let own = accounts
                    .iter()
                    .filter(|account| account.connection == connection.id)
                    .cloned()
                    .map(ProviderAccountView::from)
                    .collect();
                self.connection_view(connection, own)
            })
            .collect())
    }

    /// The provider accounts the actor may see, oldest first; only the
    /// unbound ones with `unbound`.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ReadHousehold`]; storage
    /// failures.
    pub async fn list_provider_accounts(
        &self,
        actor: &Actor,
        unbound: bool,
    ) -> Result<Vec<ProviderAccountView>, SyncError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        Ok(self
            .store
            .list_provider_accounts(None)
            .await?
            .into_iter()
            .filter(|account| visible(&access, account))
            .filter(|account| !unbound || account.account.is_none())
            .map(ProviderAccountView::from)
            .collect())
    }

    /// Connects `kind` with `input`, seals the credentials the provider
    /// returns under the new connection's id and stores it `active`,
    /// owned by the actor, with an `initial` run queued. The audit entry
    /// holds the label and provider only. With `slot`, the idempotency key
    /// is completed with the response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`];
    /// [`SyncError::Invalid`] when this server does not offer `kind`;
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
        authorize(actor, Action::ManageConnections)?;
        let provider = self
            .registry
            .get(kind)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
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
        let access = self.access.resolve(actor).await?;
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

    /// A Plaid link token to set up a new connection for the actor.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account; [`SyncError::Invalid`] when this server does
    /// not offer Plaid; [`SyncError::Provider`] when Plaid refuses; storage
    /// failures.
    pub async fn new_link_token(&self, actor: &Actor) -> Result<SecretString, SyncError> {
        authorize(actor, Action::ManageConnections)?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await?;
        let provider = self
            .registry
            .get(ProviderKind::Plaid)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        provider
            .link_token(actor.member, None)
            .await
            .map_err(SyncError::Provider)
    }

    /// A Plaid link token in update mode for the connection `id`: the member
    /// logs in to the institution again and the access token stays the same.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::confirm_reauthorized`];
    /// [`SyncError::Vault`] when the credentials cannot be opened;
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
        provider
            .link_token(actor.member, Some(&credentials))
            .await
            .map_err(SyncError::Provider)
    }

    /// Records that the member finished Plaid's update mode. The access
    /// token did not change, so nothing is resealed and the saved cursor
    /// stays valid: the connection is `active` again with its error cleared,
    /// and a `manual` run is queued when its limits allow one, which also
    /// shows whether the login really is back.
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

    /// Gives up the connection's credentials at the provider, then erases
    /// them and marks the connection `removed`. Its provider accounts,
    /// accounts, transactions and bindings stay; no sync writes anything
    /// for it once the removal commits. Credentials that cannot be opened,
    /// a provider this server no longer offers, or a provider that already
    /// refuses them leave nothing to give up, and the removal goes ahead.
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
        let Some(provider) = self.provider_of(connection) else {
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

    /// Queues a run of the connection: `manual` for an actor, `schedule`
    /// for the scheduler (no actor). A manual request is audited with its
    /// member and agent.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account; [`SyncError::Invalid`] for another trigger, or when this server does
    /// not offer the connection's provider; [`SyncError::NotFound`] for an
    /// unknown connection; [`SyncError::Conflict`] for a removed one;
    /// [`SyncError::InProgress`] while a run is queued or running;
    /// [`SyncError::RateLimited`] when the provider's limits do not allow a
    /// run yet; storage failures.
    pub async fn request_sync(
        &self,
        actor: Option<&Actor>,
        id: ConnectionId,
        trigger: SyncTrigger,
    ) -> Result<SyncRunView, SyncError> {
        match (actor, trigger) {
            (Some(actor), SyncTrigger::Manual) => {
                authorize(actor, Action::ManageConnections)?;
                self.authorize_every_account(actor, AccountLevel::Write)
                    .await?;
            }
            (None, SyncTrigger::Schedule) => {}
            _ => return Err(SyncError::Invalid(TRIGGER_INVALID)),
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let connection = ensure_not_removed(&mut tx, id).await?;
        // The schedule's snapshot was read outside this transaction: a
        // connection that needs reconnecting since is refused here. A
        // member may still try.
        if trigger == SyncTrigger::Schedule && connection.status == ConnectionStatus::ReauthRequired
        {
            return Err(SyncError::Conflict(NEEDS_RECONNECT));
        }
        let provider = self
            .provider_of(&connection)
            .ok_or(SyncError::Invalid(PROVIDER_NOT_OFFERED))?;
        check_limits(&mut tx, id, trigger, provider.capabilities(), now).await?;
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: id,
                trigger,
                requested_by: actor.map(requester),
                queued_at: now,
            })
            .await?;
        if let Some(actor) = actor {
            audit(
                &mut tx,
                now,
                actor,
                "sync.request",
                ("connection", id.to_string()),
                AuditChanges::new()
                    .set("run", run.id.to_string())
                    .set("trigger", trigger.as_str()),
            )
            .await?;
        }
        tx.commit().await?;
        self.queued.notify_one();
        tracing::info!(connection = %id, run = %run.id, trigger = trigger.as_str(), "sync requested");
        Ok(SyncRunView::from(run))
    }

    /// One page of the connection's runs, newest first.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ReadHousehold`] or
    /// transaction access to every account;
    /// [`SyncError::NotFound`] for an unknown connection;
    /// [`SyncError::Store`] with [`coffer_store::StoreError::InvalidCursor`]
    /// for a cursor this method did not issue; storage failures.
    pub async fn list_runs(
        &self,
        actor: &Actor,
        id: ConnectionId,
        page: &PageRequest,
    ) -> Result<Page<SyncRunView>, SyncError> {
        authorize(actor, Action::ReadHousehold)?;
        // A run's counts and errors cover every account the connection feeds.
        self.authorize_every_account(actor, AccountLevel::Transactions)
            .await?;
        self.store.get_connection(id).await?;
        let page = self.store.list_sync_runs(id, page).await?;
        Ok(Page {
            items: page.items.into_iter().map(SyncRunView::from).collect(),
            next: page.next,
        })
    }

    /// Binds the provider account to `account` and turns the records held
    /// while it was unbound into transactions, in one transaction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without write access to the account and to
    /// every account, or without [`Action::ManageConnections`] (a hidden
    /// account answers like an unknown one); [`SyncError::NotFound`] for
    /// an unknown provider account or account; [`SyncError::Conflict`] when
    /// the provider account is already bound, its connection is removed or
    /// the account is closed; [`SyncError::CurrencyMismatch`] when the
    /// currencies differ; storage failures.
    pub async fn bind(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
        account: AccountId,
    ) -> Result<ProviderAccountView, SyncError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, account, AccountLevel::Write)?;
        authorize_every_account(actor, &access, AccountLevel::Write)?;
        authorize(actor, Action::ManageConnections)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let view = bind_in(&mut tx, now, actor, &access, provider_account, account).await?;
        tx.commit().await?;
        tracing::info!(provider_account = %provider_account, account = %account, "provider account bound");
        Ok(view)
    }

    /// Creates an account for the provider account and binds it, in one
    /// transaction: kind from the input, else the provider's hint; name
    /// from the input, else the provider account's; currency the provider
    /// account's. The account is created exactly as
    /// [`Ledger::create_account`] creates one, audit included.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::WriteHousehold`],
    /// [`Action::ManageConnections`] or write access to every account; [`SyncError::NotFound`] for an
    /// unknown provider account; [`SyncError::Invalid`] without a kind or
    /// for an invalid name; [`SyncError::CurrencyMismatch`] when the
    /// provider's currency is not a supported ISO 4217 code; the errors of
    /// [`SyncService::bind`].
    pub async fn create_account_and_bind(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
        input: NewLinkedAccountInput,
    ) -> Result<LinkedAccountView, SyncError> {
        authorize(actor, Action::WriteHousehold)?;
        authorize(actor, Action::ManageConnections)?;
        if let Some(name) = &input.name {
            parse_name(name)?;
        }
        let access = self.access.resolve(actor).await?;
        authorize_every_account(actor, &access, AccountLevel::Write)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_provider_account(provider_account).await?;
        if !visible(&access, &current) {
            return Err(SyncError::NotFound);
        }
        if current.account.is_some() {
            return Err(SyncError::Conflict(ALREADY_BOUND));
        }
        let kind = input
            .kind
            .or(current.kind_hint)
            .ok_or(SyncError::Invalid(KIND_REQUIRED))?;
        let currency =
            Currency::parse(&current.currency_raw).map_err(|_| SyncError::CurrencyMismatch)?;
        let name = if let Some(name) = input.name {
            name
        } else {
            let name = default_account_name(&current);
            if parse_name(&name).is_err() {
                return Err(SyncError::Invalid(NAME_REQUIRED));
            }
            name
        };
        let valid = ValidNewAccount::parse(NewAccountInput {
            name,
            kind,
            currency,
            liquidity: input.liquidity,
            include_in_net_worth: input.include_in_net_worth,
            include_in_forecast: input.include_in_forecast,
            opening_balance: None,
        })?;
        let (record, _) = Ledger::insert_account(&mut tx, now, actor, valid).await?;
        authorize_account(actor, &access, record.id, AccountLevel::Write)?;
        let bound = bind_in(&mut tx, now, actor, &access, provider_account, record.id).await?;
        tx.commit().await?;
        tracing::info!(provider_account = %provider_account, account = %record.id, "account created and bound");
        Ok(LinkedAccountView {
            // Created without an opening balance; held records bring
            // transactions, and its balance comes with the next sync.
            account: AccountView::new(&record, None, true),
            provider_account: bound,
        })
    }
}

/// Binds `provider_account` to `account` inside `tx` and applies its held
/// records; the caller has authorized the actor.
async fn bind_in(
    tx: &mut StoreTx,
    now: Timestamp,
    actor: &Actor,
    access: &AccountAccess,
    provider_account: ProviderAccountId,
    account: AccountId,
) -> Result<ProviderAccountView, SyncError> {
    let current = tx.get_provider_account(provider_account).await?;
    if !visible(access, &current) {
        return Err(SyncError::NotFound);
    }
    if current.account.is_some() {
        return Err(SyncError::Conflict(ALREADY_BOUND));
    }
    ensure_not_removed(tx, current.connection).await?;
    let target = tx.get_account(account).await?;
    if target.closed_at.is_some() {
        return Err(SyncError::Conflict(ACCOUNT_CLOSED));
    }
    // Two provider accounts of one connection feeding one account would
    // import every record twice, and matching never pairs records of the
    // same connection.
    if tx
        .connection_binding(current.connection, account)
        .await?
        .is_some()
    {
        return Err(SyncError::Conflict(ACCOUNT_ALREADY_FED));
    }
    let currency =
        Currency::parse(&current.currency_raw).map_err(|_| SyncError::CurrencyMismatch)?;
    if currency != target.currency {
        return Err(SyncError::CurrencyMismatch);
    }
    let bound = tx.bind_provider_account(provider_account, account).await?;
    let report = apply_held(tx, now, bound.clone(), target).await?;
    audit(
        tx,
        now,
        actor,
        "provider_account.bind",
        ("provider_account", provider_account.to_string()),
        AuditChanges::new()
            .set("account", account.to_string())
            .set("connection", current.connection.to_string()),
    )
    .await?;
    tracing::debug!(
        provider_account = %provider_account,
        created = report.created,
        attached = report.attached,
        "held records applied"
    );
    Ok(ProviderAccountView::from(bound))
}
