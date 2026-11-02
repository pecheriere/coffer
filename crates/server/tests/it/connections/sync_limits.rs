//! Rate limits and the daily budget of syncs, and what the schedule queues.

use super::{PLAINTEXT, RENEWED, setup_token, world};
use axum::http::StatusCode;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::SyncError;
use coffer_store::{ConnectionStatus, ConnectionStatusChange, PageRequest};
use jiff::SignedDuration;

#[tokio::test]
async fn rate_limit_refuses_with_retry_after() {
    let world = world().await;
    let sync = &world.server.sync;
    let hour = SignedDuration::from_hours(1);

    // 16 manual syncs, an hour apart: the daily budget.
    for _ in 0..16 {
        let run = sync
            .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
            .await
            .unwrap();
        sync.run(run.id).await.unwrap();
        world.server.clock.advance(hour);
    }
    // The 17th within 24 h waits until the first leaves the window.
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_hours(8)
    ));
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_hours(8)
    ));
    world.server.clock.advance(SignedDuration::from_hours(8));
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(world.audit_actions("sync.request").await.len(), 17);
}

#[tokio::test]
async fn manual_and_scheduled_syncs_keep_their_minimum_interval() {
    let world = world().await;
    let sync = &world.server.sync;
    let run = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(run.trigger, SyncTrigger::Manual);
    assert_eq!(run.status, SyncRunStatus::Queued);
    assert_eq!(
        run.requested_by.as_ref().and_then(|by| by.member),
        Some(world.alice.member)
    );
    sync.run(run.id).await.unwrap();

    world.server.clock.advance(SignedDuration::from_mins(30));
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_mins(30)
    ));
    world.server.clock.advance(SignedDuration::from_mins(60));
    // An hour and a half after the last start: a manual sync may run, a
    // scheduled one waits for two hours.
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_mins(30)
    ));
    let scheduled = {
        world.server.clock.advance(SignedDuration::from_mins(30));
        sync.request_sync(None, world.connection, SyncTrigger::Schedule)
            .await
            .unwrap()
    };
    assert_eq!(scheduled.requested_by, None);
    // Only manual requests are audited.
    assert_eq!(world.audit_actions("sync.request").await.len(), 1);

    // Any other pairing of requester and trigger is refused.
    for (actor, trigger) in [
        (None, SyncTrigger::Manual),
        (Some(&world.alice), SyncTrigger::Schedule),
        (Some(&world.alice), SyncTrigger::Initial),
    ] {
        let err = sync
            .request_sync(actor, world.connection, trigger)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SyncError::Invalid(_)));
    }
}

#[tokio::test]
async fn second_request_while_queued_refused() {
    let world = world().await;
    let sync = &world.server.sync;
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::InProgress));
    let problem = coffer_server::http::problem::Problem::from(err);
    assert_eq!(problem.status, StatusCode::CONFLICT);
    assert_eq!(
        problem.code,
        coffer_server::http::problem::ProblemCode::SyncInProgress
    );
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::InProgress));
    assert_eq!(world.audit_actions("sync.request").await.len(), 1);
}

/// The schedule never queues a run of a connection that needs
/// reconnecting, even when its snapshot was older; a member still may.
#[tokio::test]
async fn schedule_refuses_a_connection_needing_reconnect() {
    let world = world().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        world.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::ReauthRequired,
            last_error: Some("the provider refused the credentials".into()),
            attempted_at: None,
            succeeded_at: None,
            at: world.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let refused = world
        .server
        .sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .err()
        .unwrap();
    assert!(matches!(refused, SyncError::Conflict(_)));
    assert!(
        world
            .server
            .sync
            .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
            .await
            .is_ok(),
        "a manual sync is still allowed"
    );
}

/// Reconnecting is no way around the limits: it queues a run only when a
/// manual request would be allowed.
#[tokio::test]
async fn reconnect_does_not_queue_a_run_beyond_the_limits() {
    let world = world().await;
    let sync = &world.server.sync;
    let run = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    // A run in flight: no second one.
    sync.reconnect(&world.alice, world.connection, setup_token(RENEWED))
        .await
        .unwrap();
    let runs = || async {
        world
            .server
            .store
            .list_sync_runs(world.connection, &PageRequest::default())
            .await
            .unwrap()
            .items
            .len()
    };
    assert_eq!(runs().await, 1);

    // Rate limited: 30 minutes after the last start.
    sync.run(run.id).await.unwrap();
    world.server.clock.advance(SignedDuration::from_mins(30));
    sync.reconnect(&world.alice, world.connection, setup_token(PLAINTEXT))
        .await
        .unwrap();
    assert_eq!(runs().await, 1);
    assert_eq!(world.audit_actions("connection.reconnect").await.len(), 2);
}

/// The 24-hour window and the wait agree: a run started exactly 24 hours
/// ago no longer counts.
#[tokio::test]
async fn daily_budget_frees_exactly_after_24_hours() {
    let world = world().await;
    let sync = &world.server.sync;
    let first = world.now();
    for _ in 0..16 {
        let run = sync
            .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
            .await
            .unwrap();
        sync.run(run.id).await.unwrap();
        world.server.clock.advance(SignedDuration::from_hours(1));
    }
    let day = SignedDuration::from_hours(24);
    world.server.clock.set(
        first
            .checked_add(day)
            .unwrap()
            .checked_sub(SignedDuration::from_millis(1))
            .unwrap(),
    );
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_millis(1)
    ));

    world.server.clock.set(first.checked_add(day).unwrap());
    let mut tx = world.server.store.begin().await.unwrap();
    let counts = tx
        .sync_run_counts(
            world.connection,
            world.now().checked_sub(day).unwrap(),
            world.now(),
        )
        .await
        .unwrap();
    drop(tx);
    assert_eq!(counts.started_or_pending, 15);
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
}

/// Runs once on a clock 30 days ahead, then sets the clock right again, and
/// checks that `trigger` is accepted: a run started on a clock that ran ahead
/// sets no minimum interval, or the connection would wait for that date.
async fn assert_accepted_after_a_start_in_the_future(trigger: SyncTrigger) {
    let world = world().await;
    let sync = &world.server.sync;
    let right = world.now();
    world.server.clock.set(
        right
            .checked_add(SignedDuration::from_hours(24 * 30))
            .unwrap(),
    );
    let run = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    sync.run(run.id).await.unwrap();

    world.server.clock.set(right);
    let actor = (trigger == SyncTrigger::Manual).then_some(&world.alice);
    sync.request_sync(actor, world.connection, trigger)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_start_in_the_future_does_not_hold_back_a_manual_sync() {
    assert_accepted_after_a_start_in_the_future(SyncTrigger::Manual).await;
}

#[tokio::test]
async fn a_start_in_the_future_does_not_hold_back_a_scheduled_sync() {
    assert_accepted_after_a_start_in_the_future(SyncTrigger::Schedule).await;
}

/// What the provider counts is the requests made, not the dates written down:
/// runs started on a clock that ran ahead still use the daily budget when the
/// clock is right again, so a clock that flips cannot spend it twice.
#[tokio::test]
async fn starts_in_the_future_still_use_the_daily_budget() {
    let world = world().await;
    let sync = &world.server.sync;
    let right = world.now();
    world.server.clock.set(
        right
            .checked_add(SignedDuration::from_hours(24 * 30))
            .unwrap(),
    );
    for _ in 0..16 {
        let run = sync
            .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
            .await
            .unwrap();
        sync.run(run.id).await.unwrap();
        world.server.clock.advance(SignedDuration::from_hours(1));
    }

    world.server.clock.set(right);
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::RateLimited { .. }));
}

/// The date shown for the next sync is not a date in the future that the
/// schedule no longer waits for.
#[tokio::test]
async fn next_sync_is_now_when_the_last_attempt_is_in_the_future() {
    let world = world().await;
    let right = world.now();
    world.server.clock.set(
        right
            .checked_add(SignedDuration::from_hours(24 * 30))
            .unwrap(),
    );
    world.sync_once(Vec::new()).await;

    world.server.clock.set(right);
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(views[0].next_sync_at, Some(right));
}
