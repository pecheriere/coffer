//! The scheduler and a clock that ran ahead: a connection whose last attempt
//! is later than now is synced, once, and the log says why.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::time::Duration;

use coffer_domain::ids::ConnectionId;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::sync::scheduler;
use coffer_server::testkit::{TestServer, captured_log_text, event_signal};
use coffer_store::{ConnectionStatus, ConnectionStatusChange, PageRequest};
use jiff::SignedDuration;
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(10);

/// What the ticker logs when it syncs a connection attempted in the future.
const AHEAD_LOGGED: &str = "a connection's last sync attempt is later than now, as after a clock \
                            that ran ahead; syncing it now";

struct World {
    server: TestServer,
    connection: ConnectionId,
}

/// A connection whose last attempt is 30 days after the server's clock.
async fn world_attempted_in_the_future() -> World {
    let (server, _fake) = TestServer::with_fake_provider().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let connection = server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Family bank",
            member,
            "https://user:pass@bridge.example/simplefin",
        )
        .await;
    let ahead = server
        .clock
        .now()
        .checked_add(SignedDuration::from_hours(24 * 30))
        .unwrap();
    let mut tx = server.store.begin().await.unwrap();
    tx.update_connection_status(
        connection,
        ConnectionStatusChange {
            status: ConnectionStatus::Active,
            last_error: None,
            attempted_at: Some(ahead),
            succeeded_at: Some(ahead),
            at: ahead,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    World { server, connection }
}

async fn first_scheduled_run(world: &World) -> SyncTrigger {
    tokio::time::timeout(WAIT, async {
        loop {
            let runs = world
                .server
                .store
                .list_sync_runs(world.connection, &PageRequest::default())
                .await
                .unwrap();
            if let Some(run) = runs.items.first() {
                return run.trigger;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_connection_attempted_in_the_future_is_queued_by_the_schedule() {
    let world = world_attempted_in_the_future().await;
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = scheduler::spawn(world.server.app_state(), async {
        let _ = stopped.await;
    });

    let trigger = first_scheduled_run(&world).await;
    let _ = stop.send(());
    handle.await.unwrap();

    assert_eq!(trigger, SyncTrigger::Schedule);
}

#[tokio::test]
async fn syncing_a_connection_attempted_in_the_future_is_logged_once() {
    let world = world_attempted_in_the_future().await;
    let logged = event_signal(AHEAD_LOGGED, "connection", world.connection);
    let (stop, stopped) = oneshot::channel::<()>();
    let handle = scheduler::spawn(world.server.app_state(), async {
        let _ = stopped.await;
    });

    tokio::time::timeout(WAIT, logged.notified()).await.unwrap();
    let _ = stop.send(());
    handle.await.unwrap();

    let id = world.connection.to_string();
    let lines = captured_log_text()
        .lines()
        .filter(|line| line.contains(AHEAD_LOGGED) && line.contains(&id))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("WARN"), "{}", lines[0]);
}
