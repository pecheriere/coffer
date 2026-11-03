//! The table's days, and that neither task can hold up anything else.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use coffer_domain::fx::FxSource;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::time::Clock as _;
use coffer_server::cli::{open_state, serve_with};
use coffer_server::config::Config;
use coffer_server::fx::RateProvider;
use coffer_server::sync::provider::{FetchResult, ProviderKind};
use coffer_server::sync::scheduler;
use coffer_server::testkit::{TestServer, captured_log_text, log_marker};
use coffer_store::PageRequest;
use jiff::Timestamp;
use jiff::civil::date;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{
    FAST_TICK, FRIDAY, MONDAY, Stalled, WAIT, coverage_of, filled_through, holding, hours, noon,
    rate, rows_of, scripted, service, snap, start, until,
};

#[tokio::test]
async fn the_table_fills_at_midnight() {
    let at = |text: &str| text.parse::<Timestamp>().unwrap();
    let cases = [
        // UTC+13: 11:00 UTC is midnight on the 2nd.
        (
            "Pacific/Auckland",
            at("2026-03-01T10:59:59Z"),
            date(2026, 3, 1),
            at("2026-03-01T11:00:00Z"),
            date(2026, 3, 2),
        ),
        // UTC-8: 08:00 UTC is midnight on the 1st.
        (
            "America/Los_Angeles",
            at("2026-03-01T07:59:59Z"),
            date(2026, 2, 28),
            at("2026-03-01T08:00:00Z"),
            date(2026, 3, 1),
        ),
    ];
    for (zone, before, day_before, after, day_after) in cases {
        let server = TestServer::with_env(&[("COFFER_TIMEZONE", zone)]).await;
        let fx = service(&server, None);
        server.clock.set(before);
        fx.keep_table_current().await.unwrap();
        let state = server.store.net_worth_state().await.unwrap();
        assert_eq!(state.filled_through, Some(day_before), "{zone}");

        fx.keep_table_current().await.unwrap();
        let state = server.store.net_worth_state().await.unwrap();
        assert_eq!(state.filled_through, Some(day_before), "{zone}");

        server.clock.set(after);
        fx.keep_table_current().await.unwrap();
        let state = server.store.net_worth_state().await.unwrap();
        assert_eq!(state.filled_through, Some(day_after), "{zone}");
    }
}

#[tokio::test]
async fn a_start_after_downtime_fills_every_missed_day() {
    let (server, _provider, fx) = scripted().await;
    let usd = holding(&server.store, "USD").await;
    snap(&server.store, usd, MONDAY, 100).await;
    fx.keep_table_current().await.unwrap();
    assert_eq!(rows_of(&server.store, usd).await.len(), 1);

    server.clock.advance(hours(24 * 5));
    fx.keep_table_current().await.unwrap();

    let days: Vec<_> = rows_of(&server.store, usd)
        .await
        .iter()
        .map(|row| row.on)
        .collect();
    assert_eq!(days.len(), 6);
    assert_eq!(days.last(), Some(&date(2026, 11, 7)));
}

/// After a clock that ran ahead the table stays until the day catches up: a fill
/// never moves back, and only `coffer rebuild-net-worth` undoes it.
#[tokio::test]
async fn a_table_ahead_of_today_is_left_alone() {
    let (server, _provider, fx) = scripted().await;
    let usd = holding(&server.store, "USD").await;
    snap(&server.store, usd, MONDAY, 100).await;
    server.clock.advance(hours(48));
    fx.keep_table_current().await.unwrap();

    server.clock.set(noon(MONDAY));
    fx.keep_table_current().await.unwrap();

    let state = server.store.net_worth_state().await.unwrap();
    assert_eq!(state.filled_through, Some(date(2026, 11, 4)));
    assert_eq!(rows_of(&server.store, usd).await.len(), 3);
}

/// How many warnings since `start` name the command and the table's last day.
fn table_warnings(start: &str, through: &str) -> usize {
    let logged = captured_log_text();
    let (_, after) = logged.split_once(start).unwrap();
    after
        .lines()
        .filter(|line| {
            line.contains("WARN")
                && line.contains("coffer rebuild-net-worth")
                && line.contains(through)
        })
        .count()
}

/// A clock ahead at first start leaves the table's last day in the future, where
/// nothing moves it back: that is said, once a day and not on every pass.
#[tokio::test]
async fn a_table_far_ahead_warns_once_a_day() {
    let server = TestServer::new().await;
    let fx = service(&server, None);
    server.clock.set(noon(date(2044, 3, 3)));
    fx.keep_table_current().await.unwrap();
    server.clock.set(noon(MONDAY));
    let start_log = log_marker();

    for _ in 0..3 {
        fx.keep_table_current().await.unwrap();
    }
    assert_eq!(table_warnings(&start_log, "2044-03-03"), 1);

    server.clock.advance(hours(24));
    for _ in 0..2 {
        fx.keep_table_current().await.unwrap();
    }
    assert_eq!(table_warnings(&start_log, "2044-03-03"), 2);

    server
        .store
        .rebuild_net_worth(date(2026, 11, 3))
        .await
        .unwrap();
    server.clock.advance(hours(24));
    fx.keep_table_current().await.unwrap();
    assert_eq!(table_warnings(&start_log, "2044-03-03"), 2);
}

/// A day ahead is what a changed time zone can do. More is a clock.
#[tokio::test]
async fn a_table_one_day_ahead_says_nothing() {
    let server = TestServer::new().await;
    let fx = service(&server, None);
    server.clock.set(noon(date(2041, 5, 6)));
    fx.keep_table_current().await.unwrap();
    let start_log = log_marker();

    server.clock.set(noon(date(2041, 5, 5)));
    fx.keep_table_current().await.unwrap();
    assert_eq!(table_warnings(&start_log, "2041-05-06"), 0);

    server.clock.set(noon(date(2041, 5, 4)));
    fx.keep_table_current().await.unwrap();
    assert_eq!(table_warnings(&start_log, "2041-05-06"), 1);
}

#[tokio::test]
async fn the_migration_s_rebuild_runs_at_start() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let usd = holding(&server.store, "USD").await;
    snap(&server.store, usd, date(2026, 9, 23), 100).await;
    assert!(server.store.net_worth_state().await.unwrap().rebuild_needed);
    let start_log = log_marker();

    let running = start(service(&server, None));
    filled_through(&server.store, MONDAY).await;
    // Another connection sees the commit before the job is woken to log it, and stopping then
    // would drop the job with the line unwritten.
    until(|| async {
        let logged = captured_log_text();
        let (_, after) = logged.split_once(&start_log).unwrap();
        after
            .lines()
            .any(|line| line.contains("INFO") && line.contains("rows=41"))
    })
    .await;
    running.stop().await;

    let state = server.store.net_worth_state().await.unwrap();
    assert!(!state.rebuild_needed);
    assert_eq!(rows_of(&server.store, usd).await.len(), 41);
}

/// A rebuild a crash left part way goes on where it stopped, up to the day it began with,
/// and the pass after it fills the days since.
#[tokio::test]
async fn a_rebuild_left_part_way_is_finished() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let (one, two) = (
        holding(&server.store, "USD").await,
        holding(&server.store, "USD").await,
    );
    let started = date(2026, 9, 30);
    for account in [one, two] {
        snap(&server.store, account, date(2026, 9, 23), 100).await;
    }
    let mut tx = server.store.begin().await.unwrap();
    let first = tx.rebuild_net_worth_batch(started, 1).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!first.finished);
    let fx = service(&server, None);

    fx.keep_table_current().await.unwrap();

    let state = server.store.net_worth_state().await.unwrap();
    assert!(!state.rebuild_needed);
    assert_eq!(state.filled_through, Some(started));
    assert_eq!(rows_of(&server.store, two).await.len(), 8);

    fx.keep_table_current().await.unwrap();

    assert_eq!(
        server.store.net_worth_state().await.unwrap().filled_through,
        Some(MONDAY)
    );
    assert_eq!(rows_of(&server.store, one).await.len(), 41);
    assert_eq!(rows_of(&server.store, two).await.len(), 41);
}

/// With `coffer rebuild-net-worth` at work the job leaves the rebuild to it, and fails
/// nothing; if the command dies the job takes it from there.
#[tokio::test]
async fn the_job_leaves_a_rebuild_to_the_command() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let usd = holding(&server.store, "USD").await;
    snap(&server.store, usd, date(2026, 9, 23), 100).await;
    let held = server
        .store
        .hold_net_worth_rebuild_for_tests()
        .unwrap()
        .unwrap();
    let fx = service(&server, None);
    let start_log = log_marker();
    let said = |start: &str| {
        let logged = captured_log_text();
        let (_, after) = logged.split_once(start).unwrap();
        after
            .lines()
            .filter(|line| line.contains("INFO") && line.contains("another process is running"))
            .count()
    };

    fx.keep_table_current().await.unwrap();
    fx.keep_table_current().await.unwrap();

    assert!(server.store.net_worth_state().await.unwrap().rebuild_needed);
    assert_eq!(rows_of(&server.store, usd).await.len(), 0);
    assert_eq!(said(&start_log), 1, "once, not every pass");

    drop(held);
    fx.keep_table_current().await.unwrap();

    assert!(!server.store.net_worth_state().await.unwrap().rebuild_needed);
    assert_eq!(rows_of(&server.store, usd).await.len(), 41);

    // The next runner gets a line of its own.
    let held = server
        .store
        .hold_net_worth_rebuild_for_tests()
        .unwrap()
        .unwrap();
    server
        .store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET rebuild_needed = 1 WHERE id = ?",
            "1",
        )
        .await
        .unwrap();
    fx.keep_table_current().await.unwrap();
    assert_eq!(said(&start_log), 2);
    drop(held);
}

/// The table's day moves while a fetch hangs: two tasks, not one.
#[tokio::test]
async fn a_stalled_provider_does_not_stop_the_fill() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    let provider = Arc::new(Stalled::default());

    let running = start(service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    ));
    until(|| async { provider.asked.load(Ordering::SeqCst) }).await;
    filled_through(&server.store, MONDAY).await;

    server.clock.advance(hours(24));
    filled_through(&server.store, date(2026, 11, 3)).await;
    running.stop().await;
}

#[tokio::test]
async fn a_stalled_provider_does_not_stop_a_sync() {
    let (server, fake) = TestServer::with_fake_provider().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let connection = server
        .seed_connection(ProviderKind::SimpleFin, "Family bank", member, "scripted")
        .await;
    fake.push(Ok(FetchResult {
        accounts: Vec::new(),
        connection_errors: Vec::new(),
        skipped: 0,
        cursor: None,
    }));
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    let provider = Arc::new(Stalled::default());
    let running = start(service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    ));
    until(|| async { provider.asked.load(Ordering::SeqCst) }).await;

    let (stop, stopped) = oneshot::channel::<()>();
    let sync = scheduler::spawn(server.app_state(), async {
        let _ = stopped.await;
    });
    let finished = |server: &TestServer| {
        let store = server.store.clone();
        async move {
            store
                .list_sync_runs(connection, &PageRequest::default())
                .await
                .unwrap()
                .items
                .into_iter()
                .find(|run| run.finished_at.is_some())
        }
    };
    until(|| async { finished(&server).await.is_some() }).await;

    assert_eq!(
        finished(&server).await.unwrap().status,
        SyncRunStatus::Succeeded
    );
    stop.send(()).unwrap();
    sync.await.unwrap();
    running.stop().await;
}

/// An attempt is committed before the call, so a crash or a stop during a fetch
/// can't make the next start ask again at once.
#[tokio::test]
async fn a_fetch_cut_short_still_counts_as_an_attempt() {
    let server = TestServer::new().await;
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    let provider = Arc::new(Stalled::default());
    let running = start(service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    ));
    until(|| async { provider.asked.load(Ordering::SeqCst) }).await;
    running.stop().await;

    let attempted = server.store.fx_coverage().await.unwrap();
    assert_eq!(attempted.len(), 1, "the first currency's attempt");
    assert_eq!(attempted[0].last_attempt_at, server.clock.now());
}

/// Whatever a fetch is doing, shutdown doesn't wait for it.
#[tokio::test]
async fn a_stalled_provider_does_not_hold_up_shutdown() {
    let server = TestServer::new().await;
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    let provider = Arc::new(Stalled::default());
    let running = start(service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    ));
    until(|| async { provider.asked.load(Ordering::SeqCst) }).await;

    running.stop().await;
}

#[tokio::test]
async fn with_no_provider_days_still_fill_and_no_rate_is_tried() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;

    let running = start(service(&server, None));
    filled_through(&server.store, MONDAY).await;
    server.clock.advance(hours(24));
    filled_through(&server.store, date(2026, 11, 3)).await;
    running.stop().await;

    assert_eq!(server.store.fx_coverage().await.unwrap(), []);
}

/// A panic in a fetch ends that pass only: the task starts again, the table's
/// task never notices, and the log doesn't hold what the panic said.
#[tokio::test]
async fn a_panic_in_a_fetch_starts_the_task_again() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    provider.panic_next.store(true, Ordering::SeqCst);
    let start_log = log_marker();

    let running = start(fx);
    filled_through(&server.store, MONDAY).await;
    server.clock.advance(hours(24));
    filled_through(&server.store, date(2026, 11, 3)).await;
    until(|| async { coverage_of(&server.store, "USD").await.is_some() }).await;
    running.stop().await;

    let logged = captured_log_text();
    let (_, after) = logged.split_once(&start_log).unwrap();
    assert!(
        after
            .lines()
            .any(|line| line.contains("exchange rates") && line.contains("starting it again")),
        "the panic is not logged"
    );
    assert!(!after.contains("7c1e"), "a log holds the panic's message");
}

/// A store failure ends a pass, not a task: each task tries again and gets there
/// once the store reads again.
#[tokio::test]
async fn a_store_failure_is_retried() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    let store = &server.store;
    store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET filled_through = ?",
            "not a stored day",
        )
        .await
        .unwrap();
    store
        .execute_unchecked_for_tests(
            "INSERT INTO fx_coverage (currency, status, last_attempt_at) VALUES (?, 'odd', 0)",
            "CHF",
        )
        .await
        .unwrap();
    assert!(fx.keep_table_current().await.is_err());
    assert!(fx.fetch_due().await.is_err());

    let running = start(fx);
    tokio::time::sleep(Duration::from_millis(50)).await;
    store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET filled_through = NULL WHERE filled_through = ?",
            "not a stored day",
        )
        .await
        .unwrap();
    store
        .execute_unchecked_for_tests("DELETE FROM fx_coverage WHERE currency = ?", "CHF")
        .await
        .unwrap();

    filled_through(store, MONDAY).await;
    until(|| async { coverage_of(store, "USD").await.is_some() }).await;
    running.stop().await;
}

/// Serves on a port with `fx` as its rate jobs, with the table waiting for its
/// rebuild as after the migration; answers while rates hang.
#[tokio::test]
async fn serving_does_not_wait_for_rates_or_the_table() {
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let usd = holding(&server.store, "USD").await;
    holding(&server.store, "GBP").await;
    snap(&server.store, usd, FRIDAY, 100).await;
    let provider = Arc::new(Stalled::default());
    let mut state = server.app_state();
    state.fx = Arc::new(
        service(
            &server,
            Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
        )
        .with_tick(FAST_TICK),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = tokio::spawn(serve_with(state, listener, async {
        let _ = stopped.await;
    }));

    until(|| async { provider.asked.load(Ordering::SeqCst) }).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "GET /healthz");
    filled_through(&server.store, MONDAY).await;

    stop.send(()).unwrap();
    tokio::time::timeout(WAIT, serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// The wiring `serve` runs on: the ECB unless told otherwise, and nobody then.
#[tokio::test]
async fn production_wiring_registers_the_configured_rate_source() {
    for (provider, expected) in [(None, Some(FxSource::Ecb)), (Some("none"), None)] {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let data_dir = dir.path().to_str().unwrap().to_owned();
        let config = Config::from_env(|name| match name {
            "COFFER_PUBLIC_URL" => Some("https://coffer.test".to_owned()),
            "COFFER_DATA_DIR" => Some(data_dir.clone()),
            "COFFER_FX_PROVIDER" => provider.map(str::to_owned),
            _ => None,
        })
        .unwrap();

        let state = open_state(config).await.unwrap();

        assert_eq!(state.fx.rate_source(), expected);
    }
}
