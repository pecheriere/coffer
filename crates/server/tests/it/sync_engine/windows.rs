//! The window a run fetches and the household's `today`.

use super::{World, fetched, provider_account, txn, world, world_on};
use coffer_domain::money::DecimalAmount;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::IanaZone;
use coffer_server::ledger::settings::ForecastSettingsPatch;
use coffer_server::ledger::transactions::Patch;
use coffer_server::testkit::browser::Browser;
use coffer_server::testkit::{INJECTED_FAILURE_RECORD_ID, TestServer};
use jiff::civil::date;
use jiff::{SignedDuration, Span, Timestamp};
use secrecy::ExposeSecret as _;

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
        Some(first_today.checked_sub(Span::new().days(88)).unwrap()),
        "the first run asks for the provider's maximum window"
    );
    let success_day = first_success
        .to_zoned(world.server.config.default_timezone.clone())
        .date();
    assert_eq!(
        requests[1].window_start,
        Some(success_day.checked_sub(Span::new().days(14)).unwrap()),
        "later runs overlap the last success"
    );
    assert_eq!(
        requests[2].window_start,
        Some(world.days_ago(88)),
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
        let start = world.days_ago(88);
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
        Some(world.days_ago(88 + 10)),
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

/// A change of the household's zone reaches the next run, with no restart, and the times
/// on the connections page with it.
#[tokio::test]
async fn a_new_zone_reaches_the_next_run_and_the_page() {
    let mut server = TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Auckland")]).await;
    let fake = server.use_fake_provider();
    let world = world_on(server, fake).await;
    world
        .server
        .clock
        .set("2026-10-31T12:00:00Z".parse::<Timestamp>().unwrap());
    let session = world
        .server
        .sessions
        .create(world.alice.member)
        .await
        .unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    world.fake.push(Ok(fetched(Vec::new())));
    world.sync().await;
    assert!(
        browser
            .get("/connections")
            .await
            .body
            .contains("2026-11-01 01:00"),
        "in Auckland"
    );

    world
        .server
        .ledger
        .update_forecast_settings(
            &world.alice,
            ForecastSettingsPatch {
                timezone: Patch::Set("America/Los_Angeles".to_owned()),
                ..ForecastSettingsPatch::default()
            },
        )
        .await
        .unwrap();
    world.fake.push(Ok(fetched(Vec::new())));
    world.sync().await;

    let requests = world.fake.requests();
    assert_eq!(requests[0].today, date(2026, 11, 1));
    assert_eq!(requests[1].today, date(2026, 10, 31));
    assert_eq!(requests[1].timezone, "America/Los_Angeles");
    assert!(
        browser
            .get("/connections")
            .await
            .body
            .contains("2026-10-31 05:00"),
        "in Los Angeles"
    );
}

/// A window starts at midnight in the zone now, so a pending record dated in another zone can
/// be on its first days and still from before it. Further in, its absence means it has gone.
#[tokio::test]
async fn a_pending_record_dated_in_another_zone_can_precede_the_window() {
    let start = date(2026, 10, 10);
    for (posted_on, kept) in [
        (start, true),
        (date(2026, 10, 11), true),
        (date(2026, 10, 12), false),
    ] {
        let world = world().await;
        let held = world.account("Held", "USD").await;
        world.bound("acct-1", "USD", held).await;
        let mut pending = txn("p-1", posted_on, "-4.00");
        pending.pending = true;
        world.fake.push(Ok(fetched(vec![provider_account(
            "acct-1",
            "USD",
            start,
            vec![pending],
        )])));
        world.sync().await;
        let zone = IanaZone::parse("Pacific/Auckland").unwrap();
        let mut tx = world.server.store.begin().await.unwrap();
        tx.set_household_zone(Some(&zone), date(2099, 1, 1))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        world.fake.push(Ok(fetched(vec![provider_account(
            "acct-1",
            "USD",
            start,
            Vec::new(),
        )])));
        world.sync().await;

        assert_eq!(
            !world.transactions(held).await.is_empty(),
            kept,
            "{posted_on}"
        );
    }
}

/// With no change of zone, a pending record missing from the window has gone.
#[tokio::test]
async fn a_pending_record_in_the_zone_now_vanishes_on_the_window_s_first_day() {
    let start = date(2026, 10, 10);
    let world = world().await;
    let held = world.account("Held", "USD").await;
    world.bound("acct-1", "USD", held).await;
    let mut pending = txn("p-1", start, "-4.00");
    pending.pending = true;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        start,
        vec![pending],
    )])));
    world.sync().await;

    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        start,
        Vec::new(),
    )])));
    world.sync().await;

    assert!(world.transactions(held).await.is_empty());
}

/// A change landing during a fetch: what the run writes uses the zone at the time it writes,
/// so no balance is dated after the new today.
#[tokio::test]
async fn a_zone_changed_during_a_fetch_is_the_one_written_with() {
    let world = world().await;
    let held = world.account("Held", "USD").await;
    world.bound("acct-1", "USD", held).await;
    // Auckland is on the 1st; Los Angeles, chosen before the answer is applied, the 31st.
    let auckland = IanaZone::parse("Pacific/Auckland").unwrap();
    let los_angeles = IanaZone::parse("America/Los_Angeles").unwrap();
    world
        .server
        .clock
        .set("2026-11-01T01:00:00Z".parse::<Timestamp>().unwrap());
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_household_zone(Some(&auckland), date(2099, 1, 1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut account = provider_account("acct-1", "USD", date(2026, 10, 1), Vec::new());
    account.balance = Some((date(2026, 11, 1), DecimalAmount::parse("7.00").unwrap()));
    world.fake.push(Ok(fetched(vec![account])));
    let gate = world.fake.hold_next_fetch();

    tokio::join!(world.sync(), async {
        gate.entered().await;
        let mut tx = world.server.store.begin().await.unwrap();
        tx.set_household_zone(Some(&los_angeles), date(2099, 1, 1))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        gate.release();
    });

    let latest = world
        .server
        .store
        .latest_balance(held)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.on, date(2026, 10, 31));
    assert_eq!(
        world.fake.requests()[0].today,
        date(2026, 11, 1),
        "the window's day"
    );
}

/// A provider asked for a window has no cursor to hold, so an account that
/// keeps failing is no hold, however many syncs it fails.
#[tokio::test]
async fn a_window_provider_has_no_cursor_to_hold() {
    let world = world().await;
    let failing = world.account("Failing", "USD").await;
    world.bound("acct-x", "USD", failing).await;
    world.server.inject_source_insert_failure().await;
    for _ in 0..4 {
        world.fake.push(Ok(fetched(vec![provider_account(
            "acct-x",
            "USD",
            world.days_ago(88),
            vec![txn(INJECTED_FAILURE_RECORD_ID, world.days_ago(1), "-3")],
        )])));
        assert_eq!(world.sync().await.status, SyncRunStatus::Partial);
    }

    assert_eq!(world.provider_account("acct-x").await.held_syncs, 0);
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert!(views.iter().all(|view| view.held_cursor.is_none()));
}
