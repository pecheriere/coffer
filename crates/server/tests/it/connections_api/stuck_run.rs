//! A run whose task ended, by a panic or a storage error, while the store
//! would not take its failure: the worker keeps trying, so the run does not
//! stay marked running and the connection can sync again.

use coffer_domain::ids::{ConnectionId, SyncRunId};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::{
    InjectedRunUpdate, SCRIPTED_PANIC, captured_log_text, event_signal, log_marker,
};
use coffer_store::{NewSyncRun, QueuedRun};
use jiff::SignedDuration;

use super::{PLAINTEXT, WAIT, World, world};

/// What the worker logs each time the store refuses to mark a run failed.
const RETRY_LOGGED: &str = "could not mark the sync run failed; trying again";

/// Lets the connection's minimum interval between syncs pass, then checks it
/// accepts a sync, not answering that one is in progress, and performs it.
async fn assert_syncs_again(world: &World) {
    world.server.clock.advance(SignedDuration::from_hours(2));
    world.script_one_account();
    let again = world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(
        world.finished(again.id).await.status,
        SyncRunStatus::Succeeded
    );
}

#[tokio::test]
async fn panicked_run_is_failed_once_the_store_recovers() {
    let world = world().await;
    world.fake.panic_next_fetch();
    let panicking = world.queue_run().await;
    let start = log_marker();
    let retried = event_signal(RETRY_LOGGED, "run", panicking);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Finish)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    assert_eq!(
        world.run_record(panicking).await.status,
        SyncRunStatus::Running,
        "the store has not let the run be failed yet"
    );
    world.server.clear_sync_run_failures().await;
    let failed = world.finished(panicking).await;
    assert_syncs_again(&world).await;
    running.stop().await;

    assert_eq!(failed.status, SyncRunStatus::Failed);
    assert_eq!(
        failed.detail.as_deref(),
        Some("the sync failed on the server")
    );
    let log = captured_log_text();
    let (_, since_start) = log.split_once(&start).unwrap();
    assert!(
        !since_start.contains(SCRIPTED_PANIC),
        "the panic's message stays out of the log"
    );
}

/// The same when the run ended with a storage error: here the store refuses
/// to record the run's own outcome too.
#[tokio::test]
async fn run_that_ended_in_a_store_error_is_failed_once_the_store_recovers() {
    let world = world().await;
    world.script_one_account();
    let queued = world.queue_run().await;
    let retried = event_signal(RETRY_LOGGED, "run", queued);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Finish)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    world.server.clear_sync_run_failures().await;
    let failed = world.finished(queued).await;
    assert_syncs_again(&world).await;
    running.stop().await;

    assert_eq!(failed.status, SyncRunStatus::Failed);
}

/// A store that stays down does not hold up shutdown, and the next start
/// recovers the run it left.
#[tokio::test]
async fn shutdown_ends_the_retries_and_the_next_start_recovers_the_run() {
    let world = world().await;
    world.fake.panic_next_fetch();
    let panicking = world.queue_run().await;
    let retried = event_signal(RETRY_LOGGED, "run", panicking);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Finish)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    running.stop().await;
    world.server.clear_sync_run_failures().await;

    assert_eq!(
        world.run_record(panicking).await.status,
        SyncRunStatus::Running
    );
    let restarted = world.spawn_scheduler();
    let recovered = world.finished(panicking).await;
    restarted.stop().await;
    assert_eq!(recovered.detail.as_deref(), Some("interrupted"));
}

/// A run on `connection`, queued directly.
async fn queue_on(world: &World, connection: ConnectionId) -> SyncRunId {
    let mut tx = world.server.store.begin().await.unwrap();
    let run = tx
        .insert_sync_run(NewSyncRun {
            connection,
            trigger: SyncTrigger::Manual,
            requested_by: Some((world.alice.member, None)),
            queued_at: world.server.clock.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run.id
}

/// A run that failed in `perform` with `NotFound`, after it started, is
/// failed as any other: the kind of error says nothing of the run's state.
#[tokio::test]
async fn run_that_failed_with_not_found_after_starting_is_failed_once_the_store_recovers() {
    let world = world().await;
    let queued = world.queue_run().await;
    world.server.delete_connection_row(world.connection).await;
    let retried = event_signal(RETRY_LOGGED, "run", queued);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Finish)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    assert_eq!(
        world.run_record(queued).await.status,
        SyncRunStatus::Running
    );
    world.server.clear_sync_run_failures().await;
    let failed = world.finished(queued).await;
    running.stop().await;

    assert_eq!(failed.status, SyncRunStatus::Failed);
}

/// A run that could not even start stays queued, and is performed once the
/// store allows it, without being failed.
#[tokio::test]
async fn run_that_could_not_start_stays_queued_and_is_performed_later() {
    let world = world().await;
    world.script_one_account();
    let queued = world.queue_run().await;
    let refused = event_signal("a queued sync run could not be performed", "run", queued);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Start)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, refused.notified())
        .await
        .unwrap();
    assert_eq!(world.run_record(queued).await.status, SyncRunStatus::Queued);
    world.server.clear_sync_run_failures().await;
    let done = world.finished(queued).await;
    running.stop().await;

    assert_eq!(done.status, SyncRunStatus::Succeeded);
}

/// An error a retry cannot cure is given up on, so the runs of other
/// connections are still performed.
#[tokio::test]
async fn permanent_error_marking_a_run_failed_does_not_hold_up_other_connections() {
    let world = world().await;
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            PLAINTEXT,
        )
        .await;
    world.fake.panic_next_fetch();
    let panicking = world.queue_run().await;
    world.script_one_account();
    let next = queue_on(&world, other).await;
    world.server.inject_sync_run_failure().await;

    let running = world.spawn_scheduler();
    let done = world.finished(next).await;
    running.stop().await;

    assert_eq!(done.status, SyncRunStatus::Succeeded);
    assert_eq!(
        world.run_record(panicking).await.status,
        SyncRunStatus::Running,
        "the run it could not fail waits for a restart"
    );
}

/// A run that finished meanwhile counts as done: the worker sees it finished
/// and stops there.
#[tokio::test]
async fn run_finished_meanwhile_counts_as_done() {
    let world = world().await;
    world.fake.panic_next_fetch();
    let panicking = world.queue_run().await;
    let retried = event_signal(RETRY_LOGGED, "run", panicking);
    let done = event_signal("the sync run needed no marking failed", "run", panicking);
    world
        .server
        .inject_sync_run_fault(InjectedRunUpdate::Finish)
        .await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    world.server.clear_sync_run_failures().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.fail_sync_run(
        panicking,
        "elsewhere",
        world.server.clock.now(),
        QueuedRun::Keep,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(WAIT, done.notified()).await.unwrap();
    running.stop().await;

    assert_eq!(
        world.run_record(panicking).await.detail.as_deref(),
        Some("elsewhere")
    );
}

/// A run that can never start is failed, so the queue moves on to the other
/// connections' runs.
#[tokio::test]
async fn run_that_can_never_start_is_failed_and_the_queue_moves_on() {
    let world = world().await;
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            PLAINTEXT,
        )
        .await;
    let stuck = world.queue_run().await;
    world.script_one_account();
    let next = queue_on(&world, other).await;
    world.server.inject_sync_run_start_failure(stuck).await;

    let running = world.spawn_scheduler();
    let done = world.finished(next).await;
    let failed = world.run_record(stuck).await;
    running.stop().await;

    assert_eq!(done.status, SyncRunStatus::Succeeded);
    assert_eq!(failed.status, SyncRunStatus::Failed);
    assert_eq!(
        failed.detail.as_deref(),
        Some("the sync failed on the server")
    );
}
