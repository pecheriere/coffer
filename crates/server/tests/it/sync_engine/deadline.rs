//! The deadline on a fetch holds for every provider, here one the engine knows
//! only through the provider interface.

use std::time::Duration;

use super::{fetched, provider_account, txn, world};
use coffer_domain::sync::SyncRunStatus;
use coffer_store::ConnectionStatus;

const TOO_SLOW: &str = "the provider did not finish within the time one sync may take";

/// A fetch that does not end fails its run with a fixed text and leaves the
/// connection as any provider failure does; the next fetch is not affected.
#[tokio::test]
async fn a_fetch_that_does_not_end_fails_the_run() {
    let mut world = world().await;
    world.server.limit_fetch_time(Duration::from_millis(50));
    let _never_released = world.fake.hold_next_fetch();

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some(TOO_SLOW));
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(connection.last_error.as_deref(), Some(TOO_SLOW));
    assert_eq!(connection.last_success_at, None);

    world.server.limit_fetch_time(Duration::from_secs(20));
    let day = world.days_ago(1);
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        vec![txn("t-1", day, "-1.00")],
    )])));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Active);
}
