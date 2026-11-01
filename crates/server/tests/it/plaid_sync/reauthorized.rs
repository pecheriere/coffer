//! A reauthorization and the runs around it. Update mode keeps the access
//! token, so a run in flight has the same credentials as the connection it
//! finishes on, yet what Plaid told it may predate the new login.

use super::{GET_ACCOUNTS, LOGIN_REQUIRED, SYNC, World, page, record, world};
use axum::http::StatusCode;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::scheduler;
use coffer_store::{ConnectionStatus, PageRequest, SyncRunRecord};
use jiff::SignedDuration;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a test waits for the scheduler before failing.
const WAIT: Duration = Duration::from_secs(10);

impl World {
    /// Has a run find the login gone, then moves past the minimum interval
    /// a requested sync waits for.
    async fn login_lapsed(&self) {
        self.answer_login_required();
        self.sync().await;
        assert_eq!(
            self.connection().await.status,
            ConnectionStatus::ReauthRequired
        );
        self.server.clock.advance(SignedDuration::from_mins(16));
    }

    fn answer_login_required(&self) {
        self.script
            .reply(GET_ACCOUNTS, StatusCode::BAD_REQUEST, LOGIN_REQUIRED);
    }

    async fn confirm_reauthorized(&self) {
        self.server
            .sync
            .confirm_reauthorized(&self.alice, self.connection)
            .await
            .unwrap();
    }

    /// Performs a run that is asking Plaid when the member confirms the
    /// reauthorization and gets its answer afterwards.
    async fn run_across_confirmation(&self) -> SyncRunRecord {
        let gate = self.script.hold_next();
        let id = self.queue(self.connection).await;
        let sync = Arc::clone(&self.server.sync);
        let running = tokio::spawn(async move { sync.run(id).await });
        gate.entered().await;
        self.confirm_reauthorized().await;
        gate.release();
        running.await.unwrap().unwrap();
        self.server.store.get_sync_run(id).await.unwrap()
    }

    /// The connection's runs, newest first.
    async fn runs(&self) -> Vec<SyncRunRecord> {
        self.server
            .store
            .list_sync_runs(self.connection, &PageRequest::default())
            .await
            .unwrap()
            .items
    }
}

#[tokio::test]
async fn run_in_flight_at_confirmation_cannot_ask_for_login_again() {
    let world = world().await;
    world.login_lapsed().await;
    let before = world.connection().await;
    world.answer_login_required();

    let run = world.run_across_confirmation().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(connection.last_error, None, "the error stays cleared");
    assert_eq!(
        connection.last_attempt_at, before.last_attempt_at,
        "its attempt does not push the next scheduled sync back"
    );
    assert_eq!(
        world.runs().await.len(),
        2,
        "the run in flight blocks queueing another"
    );
}

#[tokio::test]
async fn run_in_flight_at_confirmation_cannot_report_another_failure() {
    let world = world().await;
    world.login_lapsed().await;
    world
        .script
        .reply(GET_ACCOUNTS, StatusCode::INTERNAL_SERVER_ERROR, "{}");

    let run = world.run_across_confirmation().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(connection.last_error, None);
}

/// The Item is the same, so what the run fetched is kept and its cursor stays
/// good; only its verdict on the connection is out of date.
#[tokio::test]
async fn run_in_flight_at_confirmation_keeps_its_data_and_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.login_lapsed().await;
    let before = world.connection().await;
    world.script.page(page(
        &[record("fetched", "acct-checking", "1")],
        &[],
        &[],
        "after-fetch",
        false,
    ));

    let run = world.run_across_confirmation().await;

    assert_ne!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.transactions(checking).await.len(), 1);
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("after-fetch"));
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(connection.last_attempt_at, before.last_attempt_at);
    assert_eq!(connection.last_success_at, before.last_success_at);
}

#[tokio::test]
async fn run_started_after_confirmation_can_ask_for_login_again() {
    let world = world().await;
    world.login_lapsed().await;
    world.confirm_reauthorized().await;
    let queued = world.runs().await.remove(0);
    assert_eq!(queued.status, SyncRunStatus::Queued, "confirming queues it");
    world.answer_login_required();

    world.server.sync.run(queued.id).await.unwrap();

    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::ReauthRequired);
    assert!(connection.last_error.is_some(), "and says why");
}

/// What counts is when it asked Plaid, not when it was requested.
#[tokio::test]
async fn run_queued_before_confirmation_and_started_after_can_ask_for_login_again() {
    let world = world().await;
    world.login_lapsed().await;
    let queued = world.queue(world.connection).await;
    world.confirm_reauthorized().await;
    world.answer_login_required();

    world.server.sync.run(queued).await.unwrap();

    assert_eq!(
        world.connection().await.status,
        ConnectionStatus::ReauthRequired
    );
}

/// The login may really still be gone: the run after the one that was ignored
/// says so.
#[tokio::test]
async fn run_after_the_one_in_flight_can_ask_for_login_again() {
    let world = world().await;
    world.login_lapsed().await;
    world.answer_login_required();
    world.run_across_confirmation().await;
    world.answer_login_required();

    world.sync().await;

    assert_eq!(
        world.connection().await.status,
        ConnectionStatus::ReauthRequired
    );
}

/// No run is queued while one is in flight, and the schedule then syncs the
/// connection once it is due.
#[tokio::test]
async fn schedule_syncs_the_connection_after_a_run_in_flight_at_confirmation() {
    let world = world().await;
    world.login_lapsed().await;
    world.answer_login_required();
    world.run_across_confirmation().await;
    world.script.reply(
        SYNC,
        StatusCode::OK,
        page(&[], &[], &[], "scheduled-cursor", false),
    );
    world.server.clock.advance(SignedDuration::from_hours(48));

    let (stop, stopped) = oneshot::channel::<()>();
    let scheduler = scheduler::spawn(world.server.app_state(), async {
        let _ = stopped.await;
    });
    let run = tokio::time::timeout(WAIT, async {
        loop {
            let runs = world.runs().await;
            if let Some(run) = runs
                .into_iter()
                .find(|run| run.trigger == SyncTrigger::Schedule && run.finished_at.is_some())
            {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(WAIT, scheduler)
        .await
        .unwrap()
        .unwrap();

    assert_ne!(run.status, SyncRunStatus::Failed);
    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(
        connection.provider_state.as_deref(),
        Some("scheduled-cursor")
    );
    assert!(connection.last_success_at.is_some());
}
