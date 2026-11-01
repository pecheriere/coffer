//! The window a run fetches and the household's `today`.

use super::{World, fetched, provider_account, txn, world, world_on};
use coffer_domain::sync::SyncRunStatus;
use coffer_server::testkit::{INJECTED_FAILURE_RECORD_ID, TestServer};
use jiff::civil::date;
use jiff::{SignedDuration, Span, Timestamp};

#[tokio::test]
async fn first_window_is_max_then_overlap() {
    let world = world().await;
    for _ in 0..3 {
        world.fake.push(Ok(fetched(Vec::new())));
    }

    let first = world.sync().await;
    assert_eq!(first.status, SyncRunStatus::Succeeded);
    let first_today = world.today();
    let first_success = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap()
        .last_success_at
        .unwrap();

    world.server.clock.advance(SignedDuration::from_hours(36));
    world.sync().await;

    world
        .server
        .clock
        .advance(SignedDuration::from_hours(200 * 24));
    world.sync().await;

    let requests = world.fake.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].window_start,
        Some(first_today.checked_sub(Span::new().days(90)).unwrap()),
        "the first run asks for the provider's maximum window"
    );
    let success_day = first_success
        .to_zoned(world.server.config.timezone.clone())
        .date();
    assert_eq!(
        requests[1].window_start,
        Some(success_day.checked_sub(Span::new().days(14)).unwrap()),
        "later runs overlap the last success"
    );
    assert_eq!(
        requests[2].window_start,
        Some(world.days_ago(90)),
        "after a long gap the window is clamped to the maximum"
    );
    assert!(requests.iter().all(|request| request.cursor.is_none()));
}

/// An account that fails does not move its own window: the next run asks
/// again from shortly before the account was last applied, or for the
/// maximum window when it never was.
#[tokio::test]
async fn failed_account_keeps_its_window() {
    let world = world().await;
    let failing = world.account("Failing", "USD").await;
    let healthy = world.account("Healthy", "USD").await;
    world.bound("acct-x", "USD", failing).await;
    world.bound("acct-y", "USD", healthy).await;
    world.server.inject_source_insert_failure().await;
    let day = SignedDuration::from_hours(24);
    let fetch = |world: &World, x_fails: bool| {
        let start = world.days_ago(90);
        let on = world.days_ago(1);
        let mut x = vec![txn(&format!("x-{on}"), on, "-1")];
        if x_fails {
            x.push(txn(INJECTED_FAILURE_RECORD_ID, on, "-3"));
        }
        world.fake.push(Ok(fetched(vec![
            provider_account("acct-x", "USD", start, x),
            provider_account(
                "acct-y",
                "USD",
                start,
                vec![txn(&format!("y-{on}"), on, "-2")],
            ),
        ])));
    };

    // Run 1: x fails before it was ever applied.
    fetch(&world, true);
    assert_eq!(world.sync().await.status, SyncRunStatus::Partial);
    world.server.clock.advance(day);
    // Run 2: x is applied this time.
    fetch(&world, false);
    assert_eq!(world.sync().await.status, SyncRunStatus::Succeeded);
    let x_applied = world.today();
    world.server.clock.advance(day * 5);
    // Run 3: x fails again.
    fetch(&world, true);
    assert_eq!(world.sync().await.status, SyncRunStatus::Partial);
    world.server.clock.advance(day * 5);
    // Run 4.
    fetch(&world, false);
    world.sync().await;

    let requests = world.fake.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1].window_start,
        Some(world.days_ago(90 + 10)),
        "an account never applied gets the maximum window"
    );
    assert_eq!(
        requests[2].window_start,
        Some(x_applied.checked_sub(Span::new().days(14)).unwrap()),
        "every account applied: the overlap before the last apply"
    );
    assert_eq!(
        requests[3].window_start,
        Some(x_applied.checked_sub(Span::new().days(14)).unwrap()),
        "the failed account's last apply decides, not the last run"
    );
}

#[tokio::test]
async fn fetch_request_carries_household_today() {
    let mut server = TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Auckland")]).await;
    let fake = server.use_fake_provider();
    let world = world_on(server, fake).await;
    world
        .server
        .clock
        .set("2026-10-31T12:00:00Z".parse::<Timestamp>().unwrap());
    world.fake.push(Ok(fetched(Vec::new())));
    world.sync().await;
    let requests = world.fake.requests();
    assert_eq!(requests[0].today, date(2026, 11, 1));
    assert_eq!(requests[0].timezone, "Pacific/Auckland");
}
