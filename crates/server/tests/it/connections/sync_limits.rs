//! Rate limits and the daily budget of syncs, and what the schedule queues.

use super::{PLAINTEXT, RENEWED, World, setup_token, world};
use axum::http::StatusCode;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::FetchResult;
use coffer_server::sync::scheduler::{ScheduleEntry, due};
use coffer_store::{ConnectionStatus, ConnectionStatusChange, NewSyncRun, PageRequest};
use jiff::SignedDuration;

/// How far ahead the clock of these tests ran.
const AHEAD: SignedDuration = SignedDuration::from_hours(24 * 30);

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
        .sync_run_counts(world.connection, world.now().checked_sub(day).unwrap())
        .await
        .unwrap();
    drop(tx);
    assert_eq!(counts.started_or_pending, 15);
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
}

/// Runs once on a clock 30 days ahead, then sets the clock right again, and
/// checks that `trigger` waits its minimum interval from the correction, not
/// from that date, and is then accepted.
async fn assert_waits_only_its_interval_after_a_start_in_the_future(
    trigger: SyncTrigger,
    interval: SignedDuration,
) {
    let world = world().await;
    let sync = &world.server.sync;
    let right = world.now();
    world.server.clock.set(right + AHEAD);
    let run = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    sync.run(run.id).await.unwrap();

    world.server.clock.set(right);
    let actor = (trigger == SyncTrigger::Manual).then_some(&world.alice);
    let err = sync
        .request_sync(actor, world.connection, trigger)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == interval
    ));
    world.server.clock.advance(interval);
    sync.request_sync(actor, world.connection, trigger)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_manual_sync_after_a_start_in_the_future_waits_only_its_interval() {
    assert_waits_only_its_interval_after_a_start_in_the_future(
        SyncTrigger::Manual,
        SignedDuration::from_hours(1),
    )
    .await;
}

#[tokio::test]
async fn a_scheduled_sync_after_a_start_in_the_future_waits_only_its_interval() {
    assert_waits_only_its_interval_after_a_start_in_the_future(
        SyncTrigger::Schedule,
        SignedDuration::from_hours(2),
    )
    .await;
}

/// 24 runs, a bridge's day of requests, started over two days on a clock 30
/// days ahead.
async fn day_of_starts_in_the_future(world: &World) {
    let start = world.now() + AHEAD;
    let mut tx = world.server.store.begin().await.unwrap();
    for i in 0..24 {
        let at = start + SignedDuration::from_hours(2 * i);
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: world.connection,
                trigger: SyncTrigger::Schedule,
                requested_by: None,
                queued_at: at,
            })
            .await
            .unwrap();
        tx.start_sync_run(run.id, at).await.unwrap();
        tx.finish_sync_run(run.id, SyncRunStatus::Succeeded, None, at)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn manual_refusal(world: &World) -> SyncError {
    world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .err()
        .unwrap()
}

/// Those requests were made: right after the correction they still use the
/// budget, but the wait is bounded by a day, not by the date they carry.
#[tokio::test]
async fn starts_in_the_future_use_the_budget_for_one_day_after_the_correction() {
    let world = world().await;
    day_of_starts_in_the_future(&world).await;
    let sync = &world.server.sync;
    let day = SignedDuration::from_hours(24);

    assert!(matches!(
        manual_refusal(&world).await,
        SyncError::RateLimited { retry_after } if retry_after == day
    ));
    // Moved once: the wait ages instead of starting over on each request.
    world.server.clock.advance(SignedDuration::from_hours(1));
    assert!(matches!(
        manual_refusal(&world).await,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_hours(23)
    ));
    world.server.clock.advance(SignedDuration::from_hours(23));
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
}

/// The date shown for the next sync is when the limits allow a run, not a date
/// the schedule no longer waits for.
#[tokio::test]
async fn next_sync_is_when_the_limits_allow_it_after_a_start_in_the_future() {
    let world = world().await;
    let right = world.now();
    world.server.clock.set(right + AHEAD);
    world.sync_once(Vec::new()).await;

    world.server.clock.set(right);
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    // The provider's two hours between scheduled syncs, from the correction.
    assert_eq!(
        views[0].next_sync_at,
        Some(right + SignedDuration::from_hours(2))
    );
}

#[tokio::test]
async fn next_sync_follows_the_budget_after_a_day_of_starts_in_the_future() {
    let world = world().await;
    let right = world.now();
    day_of_starts_in_the_future(&world).await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        world.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::Active,
            last_error: None,
            attempted_at: Some(right + AHEAD),
            succeeded_at: None,
            at: right,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(
        views[0].next_sync_at,
        Some(right + SignedDuration::from_hours(24))
    );
}

/// A run under the corrected clock records its attempt at that time, so the
/// schedule finds the connection due once, not on every pass.
#[tokio::test]
async fn a_run_after_the_correction_ends_the_due_attempt_in_the_future() {
    let world = world().await;
    let right = world.now();
    world.server.clock.set(right + AHEAD);
    world.sync_once(Vec::new()).await;
    world.server.clock.set(right);
    let entry = || async {
        let record = world
            .server
            .store
            .get_connection(world.connection)
            .await
            .unwrap();
        ScheduleEntry {
            id: world.connection,
            status: record.status,
            last_attempt_at: record.last_attempt_at,
            effective_interval: SignedDuration::from_hours(6),
        }
    };
    assert_eq!(due(right, &[entry().await]), vec![world.connection]);

    // The first look moves the start in the future to now; the wait is from there.
    let err = world
        .server
        .sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::RateLimited { .. }));
    world.server.clock.advance(SignedDuration::from_hours(2));
    world.fake.push(Ok(FetchResult {
        accounts: Vec::new(),
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    let run = world
        .server
        .sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .unwrap();
    world.server.sync.run(run.id).await.unwrap();

    assert_eq!(due(world.now(), &[entry().await]), vec![]);
}
