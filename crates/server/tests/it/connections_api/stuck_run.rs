//! A run whose task ended, by a panic or a storage error, while the store
//! would not take its failure: the worker keeps trying, so the run does not
//! stay marked running and the connection can sync again.

use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::testkit::{SCRIPTED_PANIC, captured_log_text, event_signal, log_marker};
use jiff::SignedDuration;

use super::{WAIT, World, world};

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
    world.server.inject_sync_run_finish_failure().await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    assert_eq!(
        world.run_record(panicking).await.status,
        SyncRunStatus::Running,
        "the store has not let the run be failed yet"
    );
    world.server.clear_sync_run_finish_failure().await;
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
    world.server.inject_sync_run_finish_failure().await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    world.server.clear_sync_run_finish_failure().await;
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
    world.server.inject_sync_run_finish_failure().await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, retried.notified())
        .await
        .unwrap();
    running.stop().await;
    world.server.clear_sync_run_finish_failure().await;

    assert_eq!(
        world.run_record(panicking).await.status,
        SyncRunStatus::Running
    );
    let restarted = world.spawn_scheduler();
    let recovered = world.finished(panicking).await;
    restarted.stop().await;
    assert_eq!(recovered.detail.as_deref(), Some("interrupted"));
}
