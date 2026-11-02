//! The accounts request: window, authentication and the default test server.

use super::{ACCOUNTS_FIRST, PASSWORD_MARKER, Reply, USER, setup_token, world, world_with_env};
use axum::http::Method;
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::{ConnectInput, ProviderError, ProviderKind};
use coffer_server::testkit::TestServer;
use jiff::civil::date;
use jiff::tz::TimeZone;
use secrecy::SecretString;

#[tokio::test]
async fn fetch_sends_window_and_basic_auth() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let requests = world.mock.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path, "/simplefin/accounts");
    // The first sync asks for 88 days before today (2027-01-15, UTC).
    let start = date(2026, 10, 19)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_second();
    assert_eq!(
        request.query.as_deref(),
        Some(format!("start-date={start}&pending=1").as_str())
    );
    assert!(request.authorization);
    assert!(!request.path.contains(USER));
    assert!(
        !request
            .query
            .as_deref()
            .unwrap_or_default()
            .contains(PASSWORD_MARKER)
    );
}

/// The window starts at midnight of its first day in the household's time
/// zone, here on the day daylight saving time ends.
#[tokio::test]
async fn start_date_is_household_midnight_across_dst() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    // Noon on 2027-01-28 in Los Angeles: 88 days back is 2026-11-01.
    world
        .server
        .clock
        .set("2027-01-28T20:00:00Z".parse().unwrap());
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));

    world.sync(connection).await;

    // Midnight of 2026-11-01 is still daylight time (UTC-7): the clocks
    // go back at 02:00.
    let midnight: jiff::Timestamp = "2026-11-01T07:00:00Z".parse().unwrap();
    let requests = world.mock.requests();
    assert_eq!(
        requests[0].query.as_deref(),
        Some(format!("start-date={}&pending=1", midnight.as_second()).as_str())
    );
}

/// The bridge refuses a window of more than 90 days and caps it. The
/// window starts at midnight, so it must start less than 90 days before
/// the moment of the request, even late in the evening, west and east of
/// UTC.
#[tokio::test]
async fn start_date_is_within_the_bridge_limit_west_and_east_of_utc() {
    for (zone, now) in [
        ("America/Los_Angeles", "2027-02-16T07:30:00Z"),
        ("Pacific/Auckland", "2027-02-15T10:30:00Z"),
    ] {
        let world = world_with_env(&[("COFFER_TIMEZONE", zone)]).await;
        let at: jiff::Timestamp = now.parse().unwrap();
        world.server.clock.set(at);
        let connection = world.seeded().await;
        world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));

        world.sync(connection).await;

        let requests = world.mock.requests();
        let query = requests[0].query.as_deref().unwrap();
        let start: i64 = query
            .strip_prefix("start-date=")
            .and_then(|rest| rest.split('&').next())
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            at.as_second() - start < 90 * 24 * 60 * 60,
            "the window must start less than 90 days before the request"
        );
    }
}

/// The default test server offers `SimpleFin` as production does, with an
/// allow-list that allows nothing: no test can reach a real bridge.
#[tokio::test]
async fn default_test_server_never_reaches_a_real_bridge() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let refused = server
        .sync
        .create_connection(
            &alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse("Family bank").unwrap(),
            ConnectInput::SimpleFinSetupToken(SecretString::from(setup_token(
                "https://bridge.simplefin.org/claim/x",
            ))),
            None,
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        refused,
        SyncError::Provider(ProviderError::Invalid(_))
    ));
}
