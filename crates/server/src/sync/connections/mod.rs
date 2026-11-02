//! Connections to data providers and the accounts they report (spec §6.2,
//! §6.11): adding, reconnecting and removing a connection, requesting a
//! sync within the provider's limits, and binding provider accounts to
//! coffer accounts.
//!
//! Credentials exist in clear only in memory: setup input goes to the
//! provider, the credentials it returns are sealed under the connection's
//! id before anything is stored, and they are opened again only to give
//! them up at the provider when the connection is removed, or, for a
//! provider that bills until removed, to test that they still open (the
//! value is dropped at once). No audit entry,
//! log line, error or view ever holds them.

use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, MemberId, ProviderAccountId};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, Action, authorize, authorize_every_account,
};
use coffer_store::{ConnectionRecord, ConnectionStatus, ProviderAccountRecord, StoreTx};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use utoipa::ToSchema;

use self::held::held_cursor;
pub use self::held::{CURSOR_HELD_TEXT, HELD_AFTER_SYNCS, HeldCursorView};
use super::SyncError;
use super::engine::SyncService;
use super::provider::ProviderKind;
use crate::http::idempotency::IdempotencySlot;
use crate::ledger::LedgerError;
use crate::ledger::accounts::AccountView;

mod binding;
mod forget;
mod held;
pub(super) mod plaid_budget;
mod reconnect;
mod remove;
mod requests;
mod setup;

const SERIALIZATION_FAILED: &str = "response serialization failed";
const CONNECTION_CHANGED: &str =
    "the connection was reconnected or removed meanwhile; retry the request";

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
    /// Set while the sync cursor is held; `null` otherwise, for a removed
    /// connection, and for an actor who does not see every account (it names
    /// provider accounts, and the last error could too). Only the connection
    /// list fills it: the responses to creating and reconnecting a
    /// connection always have `null`.
    #[schema(required)]
    pub held_cursor: Option<HeldCursorView>,
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
            held_cursor: None,
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
    /// provider accounts the actor may see. Its last error is the provider's
    /// text and could name any account it serves, so only an actor who sees
    /// every account at [`AccountLevel::Transactions`] gets it, and the same
    /// goes for the held cursor, which names provider accounts; the status
    /// still says the connection needs attention.
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
        let sees_errors = access.sees_every_account(AccountLevel::Transactions);
        Ok(connections
            .into_iter()
            .map(|connection| {
                let own: Vec<ProviderAccountRecord> = accounts
                    .iter()
                    .filter(|account| account.connection == connection.id)
                    .cloned()
                    .collect();
                let held = if sees_errors && connection.status != ConnectionStatus::Removed {
                    held_cursor(&own)
                } else {
                    None
                };
                let mut view = self.connection_view(
                    connection,
                    own.into_iter().map(ProviderAccountView::from).collect(),
                );
                view.held_cursor = held;
                if !sees_errors {
                    view.last_error = None;
                }
                view
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
}
