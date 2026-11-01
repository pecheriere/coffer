//! Requesting a sync run within a connection's limits, and listing its runs.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_domain::sync::SyncTrigger;
use coffer_store::{ConnectionStatus, NewSyncRun, Page, PageRequest, StoreTx};
use jiff::{SignedDuration, Timestamp};

use super::{audit, requester};
use crate::sync::SyncError;
use crate::sync::engine::{PROVIDER_NOT_OFFERED, SyncRunView, SyncService, ensure_not_removed};
use crate::sync::provider::Capabilities;

const NEEDS_RECONNECT: &str = "the connection needs reconnecting";
const TRIGGER_INVALID: &str =
    "a member or agent requests manual syncs; only the schedule requests scheduled ones";

/// The window the daily budget counts runs in.
const BUDGET_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// Refuses a run that the connection's limits do not allow now:
/// [`SyncError::InProgress`] while a run is queued or running, else
/// [`SyncError::RateLimited`] with the wait until both the minimum interval
/// since the last start (`min_scheduled_interval` for the schedule,
/// `min_interval` otherwise) and the daily budget allow one.
pub(super) async fn check_limits(
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

impl SyncService {
    /// Queues a run of the connection: `manual` for an actor, `schedule`
    /// for the scheduler (no actor). A manual request is audited with its
    /// member and agent.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::ManageConnections`] or write
    /// access to every account; [`SyncError::Invalid`] for another trigger,
    /// or when this server does not offer the connection's provider;
    /// [`SyncError::NotFound`] for an
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
}
