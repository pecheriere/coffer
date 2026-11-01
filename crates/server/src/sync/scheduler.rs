//! The sync scheduler (spec §6.2): queues scheduled runs of the
//! connections that are due and performs queued runs, one at a time.
//!
//! At start, runs a restart interrupted are marked failed; queued runs stay
//! queued and are drained. A worker performs the oldest queued run, woken
//! as soon as one is queued; a ticker asks every minute for a scheduled run
//! of each due connection, within the provider's limits. A run that is
//! still going at shutdown is dropped: each provider account commits in its
//! own transaction, so only whole accounts are left behind, and the next
//! start marks the run failed. Errors are logged and retried with a
//! backoff; nothing here panics the server.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use coffer_domain::ids::ConnectionId;
use coffer_domain::sync::SyncTrigger;
use coffer_store::ConnectionStatus;
use jiff::{SignedDuration, Timestamp};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::SyncError;
use super::engine::SyncService;
use crate::http::AppState;

/// The detail of a run a restart interrupted.
const INTERRUPTED: &str = "interrupted";

/// How often the ticker looks for due connections.
const TICK: Duration = Duration::from_secs(60);

/// How long an idle worker waits before looking for a queued run without
/// being woken, for a run queued by another process.
const IDLE_POLL: Duration = Duration::from_secs(60);

/// The first and the longest wait after an error.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// What the schedule needs to know about a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleEntry {
    pub id: ConnectionId,
    pub status: ConnectionStatus,
    pub last_attempt_at: Option<Timestamp>,
    /// The longer of the server's sync interval and the provider's minimum
    /// between scheduled syncs.
    pub effective_interval: SignedDuration,
}

/// The connections due for a scheduled sync at `now`: those `active` or in
/// `error` never attempted, or attempted at least `effective_interval`
/// ago. Connections that need reconnecting or are removed are never due.
#[must_use]
pub fn due(now: Timestamp, entries: &[ScheduleEntry]) -> Vec<ConnectionId> {
    entries
        .iter()
        .filter(|entry| match entry.status {
            ConnectionStatus::Active | ConnectionStatus::Error => {
                entry.last_attempt_at.is_none_or(|at| {
                    at.checked_add(entry.effective_interval)
                        .is_ok_and(|next| next <= now)
                })
            }
            ConnectionStatus::ReauthRequired | ConnectionStatus::Removed => false,
        })
        .map(|entry| entry.id)
        .collect()
}

/// Starts the scheduler over `state`'s sync service; it stops once
/// `shutdown` resolves. The handle finishes once the worker and the ticker
/// have stopped; a run still going is aborted, and its task is cancelled at
/// its next await point without being waited for.
pub fn spawn(
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    let sync = state.sync;
    tokio::spawn(async move {
        let (stop, stopped) = watch::channel(false);
        let tasks = tokio::spawn(run_tasks(sync, stopped));
        shutdown.await;
        // No receiver left means every task has already ended.
        let _ = stop.send(true);
        if let Err(err) = tasks.await {
            tracing::error!(error = %err, "the sync scheduler ended abnormally");
        }
        tracing::info!("sync scheduler stopped");
    })
}

/// Recovers interrupted runs, then runs the worker and the ticker until
/// `stop_signal` says to stop. Each runs in its own task, so a panic in one is logged and
/// leaves the other running.
async fn run_tasks(sync: Arc<SyncService>, stop_signal: watch::Receiver<bool>) {
    let mut stop = stop_signal.clone();
    if !recover(&sync, &mut stop).await {
        return;
    }
    let worker = tokio::spawn(work(Arc::clone(&sync), stop_signal.clone()));
    let ticker = tokio::spawn(tick_loop(sync, stop_signal));
    for (task, handle) in [("worker", worker), ("ticker", ticker)] {
        if let Err(err) = handle.await {
            tracing::error!(task, error = %err, "a sync scheduler task ended abnormally");
        }
    }
}

/// Resolves once the scheduler is told to stop, or its sender is gone.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    // An error means the sender is gone: stop as well.
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Waits `delay`; `false` if the scheduler was told to stop meanwhile.
async fn pause(stop: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        () = stopped(stop) => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Doubling waits after consecutive errors, from [`BACKOFF_START`] up to
/// [`BACKOFF_MAX`].
#[derive(Debug, Default)]
struct Backoff {
    next: Option<Duration>,
}

impl Backoff {
    /// The wait after one more error.
    fn next(&mut self) -> Duration {
        let delay = self.next.map_or(BACKOFF_START, |delay| {
            delay.saturating_mul(2).min(BACKOFF_MAX)
        });
        self.next = Some(delay);
        delay
    }

    fn reset(&mut self) {
        self.next = None;
    }
}

/// Marks the runs a restart interrupted failed, retrying until it works;
/// `false` if told to stop first.
async fn recover(sync: &SyncService, stop: &mut watch::Receiver<bool>) -> bool {
    let mut backoff = Backoff::default();
    loop {
        match sync.fail_interrupted_runs().await {
            Ok(0) => return true,
            Ok(runs) => {
                tracing::warn!(
                    runs,
                    "sync runs interrupted by a restart were marked failed"
                );
                return true;
            }
            Err(err) => {
                tracing::error!(error = ?err, "could not mark interrupted sync runs failed");
                if !pause(stop, backoff.next()).await {
                    return false;
                }
            }
        }
    }
}

/// Performs queued runs one at a time, oldest first, until told to stop.
async fn work(sync: Arc<SyncService>, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    loop {
        let next = match sync.store.next_queued_run().await {
            Ok(next) => next,
            Err(err) => {
                tracing::error!(error = ?err, "could not read the queued sync runs");
                if !pause(&mut stop, backoff.next()).await {
                    return;
                }
                continue;
            }
        };
        let Some(run) = next else {
            tokio::select! {
                () = stopped(&mut stop) => return,
                () = sync.queued.notified() => {}
                () = tokio::time::sleep(IDLE_POLL) => {}
            }
            continue;
        };
        // Its own task, so a panic fails this run only.
        let task = tokio::spawn({
            let sync = Arc::clone(&sync);
            async move { sync.run(run.id).await }
        });
        let abort = task.abort_handle();
        let outcome = tokio::select! {
            outcome = task => outcome,
            () = stopped(&mut stop) => {
                abort.abort();
                return;
            }
        };
        let delay = match outcome {
            Ok(Ok(_)) => {
                backoff.reset();
                continue;
            }
            // The engine logged it and marked the run failed when it could.
            Ok(Err(err)) => {
                tracing::warn!(run = %run.id, error = %err, "a queued sync run could not be performed");
                backoff.next()
            }
            Err(err) => {
                tracing::error!(run = %run.id, error = %err, "a sync run ended abnormally");
                sync.abandon(run.id).await;
                backoff.next()
            }
        };
        if !pause(&mut stop, delay).await {
            return;
        }
    }
}

/// Every [`TICK`], queues a scheduled run of each due connection, until
/// told to stop. The first tick is at once.
async fn tick_loop(sync: Arc<SyncService>, mut stop: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = stopped(&mut stop) => return,
            _ = ticker.tick() => {}
        }
        tokio::select! {
            () = stopped(&mut stop) => return,
            () = tick(&sync) => {}
        }
    }
}

/// Queues a scheduled run of each due connection. A run the limits refuse
/// is left to a later tick.
async fn tick(sync: &SyncService) {
    let entries = match sync.schedule_entries().await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = ?err, "could not read the connections to schedule");
            return;
        }
    };
    for id in due(sync.clock.now(), &entries) {
        match sync.request_sync(None, id, SyncTrigger::Schedule).await {
            Ok(_) => {}
            Err(
                err @ (SyncError::InProgress
                | SyncError::RateLimited { .. }
                | SyncError::Conflict(_)
                | SyncError::Invalid(_)
                | SyncError::NotFound),
            ) => {
                tracing::debug!(connection = %id, reason = %err, "scheduled sync refused");
            }
            Err(err) => {
                tracing::warn!(connection = %id, error = ?err, "could not queue a scheduled sync");
            }
        }
    }
}

impl SyncService {
    /// Every connection whose provider this server offers, as the schedule
    /// sees it.
    async fn schedule_entries(&self) -> Result<Vec<ScheduleEntry>, SyncError> {
        Ok(self
            .store
            .list_connections()
            .await?
            .into_iter()
            .filter(|connection| self.provider_of(connection).is_some())
            .map(|connection| ScheduleEntry {
                id: connection.id,
                status: connection.status,
                last_attempt_at: connection.last_attempt_at,
                effective_interval: self.effective_interval(&connection),
            })
            .collect())
    }

    /// Marks every running run failed as `interrupted`; returns how many
    /// there were. Queued runs stay queued.
    async fn fail_interrupted_runs(&self) -> Result<u32, SyncError> {
        let mut tx = self.store.begin().await?;
        let runs = tx.fail_running_runs(INTERRUPTED, self.clock.now()).await?;
        tx.commit().await?;
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::ids::ConnectionId;
    use coffer_store::ConnectionStatus;
    use jiff::{SignedDuration, Timestamp};

    use super::{BACKOFF_MAX, BACKOFF_START, Backoff, ScheduleEntry, due};

    #[test]
    fn due_rules() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let interval = SignedDuration::from_hours(6);
        let ago = |hours: i64| Some(now - SignedDuration::from_hours(hours));
        let just_short = Some(now - interval + SignedDuration::from_secs(1));
        let cases = [
            (ConnectionStatus::Active, None, true),
            (ConnectionStatus::Active, ago(6), true),
            (ConnectionStatus::Active, ago(7), true),
            (ConnectionStatus::Active, just_short, false),
            (ConnectionStatus::Active, Some(now), false),
            (ConnectionStatus::Error, None, true),
            (ConnectionStatus::Error, ago(6), true),
            (ConnectionStatus::Error, ago(1), false),
            (ConnectionStatus::ReauthRequired, None, false),
            (ConnectionStatus::ReauthRequired, ago(48), false),
            (ConnectionStatus::Removed, None, false),
            (ConnectionStatus::Removed, ago(48), false),
        ];
        for (status, last_attempt_at, expected) in cases {
            let entry = ScheduleEntry {
                id: ConnectionId::new(),
                status,
                last_attempt_at,
                effective_interval: interval,
            };
            assert_eq!(due(now, &[entry]) == vec![entry.id], expected);
        }
    }

    #[test]
    fn due_keeps_order_and_skips_an_unrepresentable_next_sync() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let entry = |last_attempt_at, effective_interval| ScheduleEntry {
            id: ConnectionId::new(),
            status: ConnectionStatus::Active,
            last_attempt_at,
            effective_interval,
        };
        let first = entry(None, SignedDuration::from_hours(1));
        let overflowing = entry(Some(Timestamp::MAX), SignedDuration::from_hours(1));
        let second = entry(Some(Timestamp::MIN), SignedDuration::from_hours(1));
        assert_eq!(
            due(now, &[first, overflowing, second]),
            vec![first.id, second.id]
        );
    }

    #[test]
    fn backoff_doubles_up_to_the_maximum_and_resets() {
        let mut backoff = Backoff::default();
        assert_eq!(backoff.next(), BACKOFF_START);
        assert_eq!(backoff.next(), BACKOFF_START * 2);
        for _ in 0..10 {
            backoff.next();
        }
        assert_eq!(backoff.next(), BACKOFF_MAX);
        backoff.reset();
        assert_eq!(backoff.next(), BACKOFF_START);
    }
}
