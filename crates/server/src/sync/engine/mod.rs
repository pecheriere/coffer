//! The sync engine (spec §6.2): performs a queued run of one connection.
//!
//! A run opens the connection's credentials only for the provider call,
//! asks for a window computed from when each provider account was last
//! applied and the household's `today`, then applies each provider account
//! in its own store transaction, so a failing account leaves nothing of
//! itself behind, never blocks the others, and is fetched again from where
//! it stopped. A cursor provider's cursor moves on only once every record it
//! covers was stored or can never be: a failed account's records would
//! otherwise never be reported again. The outcome is recorded on the
//! connection, its provider accounts and the run. Every provider text is
//! sanitized before it is stored, and nothing about the credentials is ever
//! logged.

use std::sync::Arc;
use std::time::Duration;

use coffer_domain::accounts::BalanceSource;
use coffer_domain::audit::{AuditChanges, AuditEntry};
use coffer_domain::ids::{AccountId, ConnectionId, SyncRunId};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock;
use coffer_store::{
    BalanceSnapshot, ConnectionRecord, ConnectionStatus, Store, StoreError, StoreTx, SyncRunRecord,
};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::Serialize;
use utoipa::ToSchema;

use super::SyncError;
use super::apply::sync_actor;
use super::connections::plaid_budget::PlaidBudget;
use super::provider::ProviderRegistry;
use crate::auth::AccessResolver;
use crate::config::Config;
use crate::ledger::transactions::AttributionView;
use crate::vault::CredentialVault;

mod accounts;
mod convert;
mod fetch;
mod results;

const RUN_FINISHED: &str = "the sync run has already finished";
pub(super) const CONNECTION_REMOVED: &str = "the connection is removed";
pub(super) const PROVIDER_NOT_OFFERED: &str =
    "the connection's provider is not offered by this server";
const INTERNAL_FAILURE: &str = "the sync failed on the server";

/// The longest one provider fetch may take: the worker runs one sync at a
/// time, so a slow upstream delays every other connection until this ends.
/// A large first Plaid sync is some tens of pages of a few seconds each.
pub const FETCH_DEADLINE: Duration = Duration::from_mins(10);

/// A sync run, as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SyncRunView {
    pub id: SyncRunId,
    pub connection: ConnectionId,
    pub trigger: SyncTrigger,
    pub status: SyncRunStatus,
    /// Who asked for it; `null` for the schedule.
    #[schema(required)]
    pub requested_by: Option<AttributionView>,
    pub queued_at: Timestamp,
    #[schema(required)]
    pub started_at: Option<Timestamp>,
    #[schema(required)]
    pub finished_at: Option<Timestamp>,
    /// What happened: counts for a run that applied data, the sanitized
    /// error for one that failed. Never a credential.
    #[schema(required)]
    pub detail: Option<String>,
}

impl From<SyncRunRecord> for SyncRunView {
    fn from(run: SyncRunRecord) -> Self {
        Self {
            id: run.id,
            connection: run.connection,
            trigger: run.trigger,
            status: run.status,
            requested_by: run.requested_by.map(|(member, agent)| AttributionView {
                member: Some(member),
                agent,
                system: None,
                at: run.queued_at,
            }),
            queued_at: run.queued_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
            detail: run.detail,
        }
    }
}

/// Performs sync runs and manages connections and their provider accounts
/// (see [`super::connections`]).
pub struct SyncService {
    pub(super) store: Store,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) vault: Arc<CredentialVault>,
    pub(super) registry: Arc<ProviderRegistry>,
    /// The only source of account access.
    pub(super) access: Arc<AccessResolver>,
    pub(super) config: Config,
    /// See [`FETCH_DEADLINE`].
    pub(super) fetch_deadline: Duration,
    /// Wakes the scheduler's worker when a run is queued.
    pub(super) queued: tokio::sync::Notify,
    /// How often each member's Link pages may call Plaid.
    pub(super) plaid_budget: PlaidBudget,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncService")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl SyncService {
    /// The service over `store`, opening credentials with `vault`,
    /// reaching providers through `registry` and resolving account access
    /// with `access`.
    #[must_use]
    pub fn new(
        store: Store,
        clock: Arc<dyn Clock>,
        vault: Arc<CredentialVault>,
        registry: Arc<ProviderRegistry>,
        access: Arc<AccessResolver>,
        config: Config,
    ) -> Self {
        Self {
            store,
            clock,
            vault,
            registry,
            access,
            config,
            fetch_deadline: FETCH_DEADLINE,
            queued: tokio::sync::Notify::new(),
            plaid_budget: PlaidBudget::default(),
        }
    }

    /// This service with fetches limited to `deadline`, so a test can reach
    /// the limit without waiting for [`FETCH_DEADLINE`].
    #[cfg(feature = "testkit")]
    #[must_use]
    pub fn with_fetch_deadline(self, deadline: Duration) -> Self {
        Self {
            fetch_deadline: deadline,
            ..self
        }
    }

    /// Performs the queued run `run` and returns it finished. A provider
    /// failure is an outcome, recorded on the run and the connection, not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`] for an unknown run; [`SyncError::InProgress`]
    /// when it is already running; [`SyncError::Invalid`] when it has
    /// finished; storage failures, after which the run is marked failed when
    /// the store allows.
    pub async fn run(&self, run: SyncRunId) -> Result<SyncRunView, SyncError> {
        let started_at = self.clock.now();
        let mut tx = self.store.begin().await?;
        let started = match tx.start_sync_run(run, started_at).await {
            Ok(started) => started,
            Err(StoreError::Conflict(_)) => {
                drop(tx);
                let current = self.store.get_sync_run(run).await?;
                return Err(if current.status == SyncRunStatus::Running {
                    SyncError::InProgress
                } else {
                    SyncError::Invalid(RUN_FINISHED)
                });
            }
            Err(err) => return Err(err.into()),
        };
        tx.commit().await?;
        tracing::info!(
            run = %run,
            connection = %started.connection,
            trigger = started.trigger.as_str(),
            "sync run started"
        );
        match self.perform(&started, started_at).await {
            Ok(finished) => {
                tracing::info!(
                    run = %run,
                    connection = %finished.connection,
                    status = finished.status.as_str(),
                    "sync run finished"
                );
                Ok(SyncRunView::from(finished))
            }
            Err(err) => {
                tracing::error!(run = %run, connection = %started.connection, error = ?err, "sync run failed");
                self.abandon(run).await;
                Err(err)
            }
        }
    }

    /// Marks `run` failed after a storage error or a panic, whether it is
    /// still queued or running, if the store allows; a run that already
    /// finished is left as it is.
    pub(super) async fn abandon(&self, run: SyncRunId) {
        let finished = async {
            let mut tx = self.store.begin().await?;
            tx.fail_sync_run(run, INTERNAL_FAILURE, self.clock.now())
                .await?;
            tx.commit().await
        }
        .await;
        if let Err(err) = finished {
            tracing::error!(run = %run, error = ?err, "could not mark the sync run failed");
        }
    }

    async fn perform(
        &self,
        run: &SyncRunRecord,
        started_at: Timestamp,
    ) -> Result<SyncRunRecord, SyncError> {
        let connection = self.store.get_connection(run.connection).await?;
        let accounts: Vec<(Timestamp, Option<Timestamp>)> = self
            .store
            .list_provider_accounts(Some(connection.id))
            .await?
            .iter()
            .map(|account| (account.last_seen_at, account.last_applied_at))
            .collect();
        let today = self.today();
        match self.fetch(&connection, &accounts, today).await {
            Ok((result, mode)) => {
                self.apply(run, &connection, result, mode, today, started_at)
                    .await
            }
            Err(failure) => {
                self.record_failure(run, &connection, failure, started_at)
                    .await
            }
        }
    }

    /// The household's current day.
    pub(super) fn today(&self) -> Date {
        self.clock
            .now()
            .to_zoned(self.config.timezone.clone())
            .date()
    }
}

/// Whether the connection, now `stored`, still holds what a run that read
/// `opened` used: it is not removed and has the same sealed credentials. A
/// reconnect while the run was fetching replaces them.
fn same_credentials(stored: &ConnectionRecord, opened: &ConnectionRecord) -> bool {
    stored.status != ConnectionStatus::Removed && stored.credentials == opened.credentials
}

/// Whether the outcome of a run that read `opened` may still be recorded on
/// the connection, now `stored`: it has the same credentials and was not
/// reauthorized since. Otherwise the run's outcome says nothing about the
/// connection as it is now.
fn still_current(stored: &ConnectionRecord, opened: &ConnectionRecord) -> bool {
    same_credentials(stored, opened) && stored.reauth_generation == opened.reauth_generation
}

/// Refuses, with [`SyncError::Conflict`] and [`CONNECTION_REMOVED`], a
/// connection that is removed.
pub(super) async fn ensure_not_removed(
    tx: &mut StoreTx,
    connection: ConnectionId,
) -> Result<ConnectionRecord, SyncError> {
    let record = tx.get_connection(connection).await?;
    if record.status == ConnectionStatus::Removed {
        return Err(SyncError::Conflict(CONNECTION_REMOVED));
    }
    Ok(record)
}

/// Writes the day's `sync` snapshot of `account`, audited when it changes
/// the day's value or source.
pub(super) async fn write_balance(
    tx: &mut StoreTx,
    now: Timestamp,
    account: AccountId,
    on: Date,
    balance_minor: i64,
) -> Result<(), SyncError> {
    let previous = tx
        .upsert_balance(BalanceSnapshot {
            account,
            on,
            balance_minor,
            source: BalanceSource::Sync,
            recorded_at: now,
        })
        .await?;
    let unchanged = previous.as_ref().is_some_and(|previous| {
        previous.balance_minor == balance_minor && previous.source == BalanceSource::Sync
    });
    if unchanged {
        return Ok(());
    }
    let changes = AuditChanges::new()
        .set("on", on)
        .change(
            "balance_minor",
            previous.as_ref().map(|p| p.balance_minor),
            balance_minor,
        )
        .change(
            "source",
            previous.as_ref().map(|p| p.source.as_str()),
            BalanceSource::Sync.as_str(),
        );
    tx.append_audit(
        now,
        &AuditEntry {
            actor: sync_actor(),
            action: "balance.set".to_owned(),
            entity_type: "account".to_owned(),
            entity_id: account.to_string(),
            changes,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
