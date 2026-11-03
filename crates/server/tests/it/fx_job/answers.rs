//! What an answer does to what is stored, and how a failure is recorded.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use coffer_domain::fx::{FxErrorCode, FxFetchStatus};
use coffer_domain::time::Clock as _;
use coffer_server::fx::{FxService, RateError, RateProvider};
use coffer_server::testkit::{MockUpstream, TestServer, captured_log_text, log_marker};
use jiff::SignedDuration;
use jiff::civil::date;

use super::{
    FRIDAY, MONDAY, SATURDAY, SUNDAY, Scripted, coverage_of, cur, display_minor, ecb_at, holding,
    hours, noon, rate, rates_of, rows_of, scripted, service, show_in, usd_holding_in_euros,
};

#[tokio::test]
async fn rates_and_rows_commit_together() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
    assert_eq!(display_minor(&server.store, usd, FRIDAY).await, None);
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2")]));

    fx.fetch_due().await.unwrap();

    assert_eq!(rates_of(&server.store, "USD").await.len(), 1);
    assert_eq!(
        display_minor(&server.store, usd, FRIDAY).await,
        Some(50_000)
    );
    assert_eq!(
        display_minor(&server.store, usd, MONDAY).await,
        Some(50_000)
    );
}

/// Through the real job, both first use and a later window that speaks for the day: an
/// admin's rate stays and wins its day, and a later published rate takes over.
#[tokio::test]
async fn a_fetch_leaves_a_manual_rate_alone() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
    let mut tx = server.store.begin().await.unwrap();
    tx.set_manual_rate(cur("USD"), FRIDAY, "4".parse().unwrap(), server.clock.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    provider.answer("USD", Ok(vec![rate(FRIDAY, "2")]));
    fx.fetch_due().await.unwrap();
    assert_eq!(
        display_minor(&server.store, usd, FRIDAY).await,
        Some(25_000),
        "it wins on its day"
    );

    server.clock.advance(hours(6));
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2.5"), rate(MONDAY, "5")]));
    fx.fetch_due().await.unwrap();

    assert_eq!(
        display_minor(&server.store, usd, SUNDAY).await,
        Some(25_000),
        "a corrected fetch changes nothing under it, and it is carried forward"
    );
    assert_eq!(
        display_minor(&server.store, usd, MONDAY).await,
        Some(20_000),
        "a later day's rate takes over"
    );
    let manual = server.store.manual_rates().await.unwrap();
    assert_eq!(manual.len(), 1);
    assert_eq!(manual[0].rate.to_string(), "4");
}

#[tokio::test]
async fn an_unchanged_answer_rewrites_no_row() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2"), rate(MONDAY, "2.5")]));
    fx.fetch_due().await.unwrap();
    server
        .store
        .execute_unchecked_for_tests(
            "UPDATE account_daily_values SET balance_minor = -999 WHERE account_id = ?",
            &usd.to_string(),
        )
        .await
        .unwrap();

    server.clock.advance(hours(6));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);

    assert!(
        rows_of(&server.store, usd)
            .await
            .iter()
            .all(|row| row.balance_minor == -999),
        "a row was written"
    );
}

/// A forged rate on a day the source doesn't publish goes as soon as an answer
/// passes that day, and the days it reached follow.
#[tokio::test]
async fn a_rate_the_answer_does_not_hold_is_removed_and_the_rows_follow() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
    provider.answer(
        "USD",
        Ok(vec![
            rate(FRIDAY, "2"),
            rate(SATURDAY, "4"),
            rate(MONDAY, "2"),
        ]),
    );
    fx.fetch_due().await.unwrap();
    assert_eq!(
        display_minor(&server.store, usd, SUNDAY).await,
        Some(25_000)
    );

    provider.answer("USD", Ok(vec![rate(FRIDAY, "2"), rate(MONDAY, "2")]));
    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    let days: Vec<_> = rates_of(&server.store, "USD")
        .await
        .iter()
        .map(|one| one.0)
        .collect();
    assert_eq!(days, [FRIDAY, MONDAY]);
    assert_eq!(
        display_minor(&server.store, usd, SATURDAY).await,
        Some(50_000)
    );
    assert_eq!(
        display_minor(&server.store, usd, SUNDAY).await,
        Some(50_000)
    );
}

/// A rate before the window asked about was not asked about.
#[tokio::test]
async fn a_rate_before_the_window_stays() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, date(2026, 9, 1)).await;
    let old = date(2026, 9, 1);
    provider.answer("USD", Ok(vec![rate(old, "2"), rate(MONDAY, "2.5")]));
    fx.fetch_due().await.unwrap();

    provider.answer("USD", Ok(vec![rate(MONDAY, "2.5")]));
    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    assert_eq!(rates_of(&server.store, "USD").await.len(), 2);
    assert_eq!(display_minor(&server.store, usd, old).await, Some(50_000));
}

/// The ECB's answer for a window with no rates is a 200 with no body. It can't
/// be that for a window holding a rate it published, so it isn't believed.
#[tokio::test]
async fn an_empty_answer_removes_nothing() {
    let (server, provider, fx) = scripted().await;
    let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2"), rate(MONDAY, "2.5")]));
    fx.fetch_due().await.unwrap();
    let before = rows_of(&server.store, usd).await;
    let start = log_marker();

    provider.answer("USD", Ok(Vec::new()));
    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    assert_eq!(rates_of(&server.store, "USD").await.len(), 2);
    assert_eq!(rows_of(&server.store, usd).await, before);
    let coverage = coverage_of(&server.store, "USD").await.unwrap();
    assert_eq!(coverage.status, FxFetchStatus::Ready);
    assert_eq!(coverage.last_on, Some(MONDAY));
    assert_eq!(coverage.last_attempt_at, server.clock.now());
    assert_eq!(
        coverage.last_success_at,
        Some(noon(MONDAY)),
        "an empty answer is not a success"
    );
    let logged = captured_log_text();
    let (_, after) = logged.split_once(&start).unwrap();
    assert!(
        after
            .lines()
            .any(|line| line.contains("WARN") && line.contains("no rates") && line.contains("USD")),
        "the contradiction is not logged"
    );
}

/// An answer cut at the end of a line reads like a good one. It says nothing of
/// the days after its last row, so their rates stay.
#[tokio::test]
async fn a_cut_answer_keeps_the_rates_after_its_last_row() {
    let (server, provider, fx) = scripted().await;
    usd_holding_in_euros(&server, &fx, FRIDAY).await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2"), rate(MONDAY, "2.5")]));
    fx.fetch_due().await.unwrap();

    provider.answer("USD", Ok(vec![rate(FRIDAY, "2")]));
    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    let days: Vec<_> = rates_of(&server.store, "USD")
        .await
        .iter()
        .map(|one| one.0)
        .collect();
    assert_eq!(days, [FRIDAY, MONDAY]);
}

#[tokio::test]
async fn a_failed_answer_removes_nothing() {
    for failure in [
        RateError::Malformed,
        RateError::Network,
        RateError::NotPublished,
        RateError::Unavailable { status: 503 },
    ] {
        let (server, provider, fx) = scripted().await;
        let usd = usd_holding_in_euros(&server, &fx, FRIDAY).await;
        provider.answer("USD", Ok(vec![rate(FRIDAY, "2"), rate(MONDAY, "2.5")]));
        fx.fetch_due().await.unwrap();
        let before = rows_of(&server.store, usd).await;

        provider.answer("USD", Err(failure));
        server.clock.advance(hours(6));
        fx.fetch_due().await.unwrap();

        assert_eq!(rates_of(&server.store, "USD").await.len(), 2, "{failure:?}");
        assert_eq!(rows_of(&server.store, usd).await, before, "{failure:?}");
        let coverage = coverage_of(&server.store, "USD").await.unwrap();
        assert_eq!(coverage.last_on, Some(MONDAY), "{failure:?}");
    }
}

/// Through the real provider, so the source's text is really there to leak.
#[tokio::test]
async fn a_failure_is_recorded_and_retried_after_an_hour() {
    let text = "rate-source-text-5d2b";
    let mock = MockUpstream::start(Router::new().fallback(get(move || async move {
        (StatusCode::SERVICE_UNAVAILABLE, text)
    })))
    .await;
    let server = TestServer::new().await;
    server.clock.set(noon(MONDAY));
    let fx = service(&server, Some(ecb_at(&mock)));
    holding(&server.store, "NOK").await;
    show_in(&server.store, "EUR").await;
    let start = log_marker();

    assert_eq!(fx.fetch_due().await.unwrap(), 1);

    let coverage = coverage_of(&server.store, "NOK").await.unwrap();
    assert_eq!(coverage.status, FxFetchStatus::Failing);
    assert_eq!(coverage.last_error, Some(FxErrorCode::Unavailable));
    assert_eq!(coverage.last_success_at, None);
    let logged = captured_log_text();
    let (_, after) = logged.split_once(&start).unwrap();
    assert!(
        after
            .lines()
            .any(|line| line.contains("WARN") && line.contains("NOK") && line.contains("failing")),
        "the failure is not logged"
    );
    assert!(!after.contains(text), "the source's text reached a log");

    server
        .clock
        .advance(hours(1) - SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 0);
    assert_eq!(mock.requests().len(), 1);

    server.clock.advance(SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);
    assert_eq!(mock.requests().len(), 2);
}

#[tokio::test]
async fn not_published_is_recorded_not_failing() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "ARS").await;
    show_in(&server.store, "EUR").await;
    provider.answer("ARS", Err(RateError::NotPublished));

    fx.fetch_due().await.unwrap();

    let coverage = coverage_of(&server.store, "ARS").await.unwrap();
    assert_eq!(coverage.status, FxFetchStatus::NotPublished);
    assert_eq!(coverage.last_error, None);

    server
        .clock
        .advance(hours(24) - SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 0);
    server.clock.advance(SignedDuration::from_secs(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);
}

#[tokio::test]
async fn an_empty_answer_with_nothing_stored_is_not_published() {
    let (server, _provider, fx) = scripted().await;
    holding(&server.store, "ARS").await;
    show_in(&server.store, "EUR").await;

    fx.fetch_due().await.unwrap();

    let coverage = coverage_of(&server.store, "ARS").await.unwrap();
    assert_eq!(coverage.status, FxFetchStatus::NotPublished);
    assert_eq!(coverage.last_on, None);
}

#[tokio::test]
async fn a_failure_for_one_currency_doesn_t_stop_the_next() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "GBP").await;
    holding(&server.store, "USD").await;
    provider.answer("GBP", Err(RateError::Network));
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));

    assert_eq!(fx.fetch_due().await.unwrap(), 2);

    let gbp = coverage_of(&server.store, "GBP").await.unwrap();
    assert_eq!(gbp.status, FxFetchStatus::Failing);
    assert_eq!(gbp.last_error, Some(FxErrorCode::Network));
    let usd = coverage_of(&server.store, "USD").await.unwrap();
    assert_eq!(usd.status, FxFetchStatus::Ready);
}

/// An empty 200 doesn't make a failing source healthy.
#[tokio::test]
async fn an_empty_answer_leaves_a_failing_source_failing() {
    let (server, provider, fx) = scripted().await;
    usd_holding_in_euros(&server, &fx, FRIDAY).await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "2")]));
    fx.fetch_due().await.unwrap();
    provider.answer("USD", Err(RateError::Network));
    server.clock.advance(hours(6));
    fx.fetch_due().await.unwrap();

    provider.answer("USD", Ok(Vec::new()));
    server.clock.advance(hours(1));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);

    let coverage = coverage_of(&server.store, "USD").await.unwrap();
    assert_eq!(coverage.status, FxFetchStatus::Failing);
    assert_eq!(coverage.last_error, Some(FxErrorCode::Network));
    assert_eq!(coverage.last_success_at, Some(noon(MONDAY)));
    assert_eq!(coverage.last_attempt_at, server.clock.now());
}

/// A store that can't write means no request at all, and the next currency is
/// still asked for.
#[tokio::test]
async fn a_store_that_can_t_record_the_attempt_means_no_request() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "GBP").await;
    holding(&server.store, "USD").await;
    provider.answer("GBP", Ok(vec![rate(FRIDAY, "0.9")]));
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    server
        .store
        .inject_fx_coverage_fault_for_tests()
        .await
        .unwrap();

    assert!(fx.fetch_due().await.is_err(), "the failure is reported");
    assert!(fx.fetch_due().await.is_err(), "the failure is reported");

    assert_eq!(provider.calls(), [(cur("USD"), None, MONDAY)]);
    let usd = coverage_of(&server.store, "USD").await.unwrap();
    assert_eq!(usd.status, FxFetchStatus::Ready);
}

/// An answer that can't be stored costs one request per interval, not one per
/// pass, and doesn't stop the next currency.
#[tokio::test]
async fn an_answer_that_can_t_be_stored_is_not_asked_for_again_at_once() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "GBP").await;
    holding(&server.store, "USD").await;
    provider.answer("GBP", Ok(vec![rate(FRIDAY, "0.9")]));
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    server.store.inject_fx_rate_fault_for_tests().await.unwrap();

    assert!(fx.fetch_due().await.is_err(), "the failure is reported");
    assert_eq!(
        provider.calls(),
        [(cur("GBP"), None, MONDAY), (cur("USD"), None, MONDAY)]
    );
    assert_eq!(rates_of(&server.store, "USD").await.len(), 1);

    assert_eq!(fx.fetch_due().await.unwrap(), 0);
    assert_eq!(provider.calls().len(), 2);

    server.clock.advance(hours(1));
    assert!(fx.fetch_due().await.is_err(), "the failure is reported");
    assert_eq!(provider.calls().len(), 3);
}

/// One clock that ran ahead must not silence a currency until that date.
#[tokio::test]
async fn a_clock_that_ran_ahead_does_not_stop_rates() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "USD").await;
    show_in(&server.store, "EUR").await;
    provider.answer("USD", Ok(vec![rate(FRIDAY, "1.1")]));
    server.clock.set(noon(date(2030, 1, 1)));
    fx.fetch_due().await.unwrap();
    provider.forget_calls();

    server.clock.set(noon(MONDAY));
    assert_eq!(fx.fetch_due().await.unwrap(), 1);
    assert_eq!(fx.fetch_due().await.unwrap(), 0, "once, not on every pass");

    assert_eq!(
        provider.calls(),
        [(cur("USD"), Some(date(2026, 10, 23)), MONDAY)]
    );
}

/// A courtesy to the source: a pass doesn't ask for currencies back to back.
#[tokio::test]
async fn a_pass_pauses_between_requests() {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, "GBP").await;
    holding(&server.store, "USD").await;
    let started = Instant::now();

    assert_eq!(fx.fetch_due().await.unwrap(), 2);

    assert!(started.elapsed() >= Duration::from_secs(1), "no pause");
    assert_eq!(provider.calls().len(), 2);
}

/// How many lines since `start` are at `level` about `code` and say `text`.
fn lines_since(start: &str, level: &str, code: &str, text: &str) -> usize {
    let logged = captured_log_text();
    let (_, after) = logged.split_once(start).unwrap();
    after
        .lines()
        .filter(|line| line.contains(level) && line.contains(code) && line.contains(text))
        .count()
}

const FETCH_FAILED: &str = "could not fetch";

/// A household holding `code` beside the euro, and a marker for the logs. Each
/// test has a currency of its own, since tests share the log.
async fn logging(code: &str) -> (TestServer, Arc<Scripted>, FxService, String) {
    let (server, provider, fx) = scripted().await;
    holding(&server.store, code).await;
    show_in(&server.store, "EUR").await;
    (server, provider, fx, log_marker())
}

/// A blocked host fails every hour for as long as it is blocked: that is one
/// warning, and repeats only at `debug`.
#[tokio::test]
async fn a_repeated_failure_warns_once() {
    let (server, provider, fx, start) = logging("SEK").await;
    provider.answer("SEK", Err(RateError::Network));

    for _ in 0..3 {
        assert_eq!(fx.fetch_due().await.unwrap(), 1);
        server.clock.advance(hours(1));
    }

    assert_eq!(lines_since(&start, "WARN", "SEK", FETCH_FAILED), 1);
    assert_eq!(lines_since(&start, "DEBUG", "SEK", FETCH_FAILED), 2);
}

/// What was already said is in the store, so a restart doesn't say it again.
#[tokio::test]
async fn a_start_repeating_a_known_failure_is_a_repeat() {
    let (server, provider, fx, start) = logging("DKK").await;
    provider.answer("DKK", Err(RateError::Network));
    fx.fetch_due().await.unwrap();

    server.clock.advance(hours(1));
    let restarted = service(
        &server,
        Some(Arc::clone(&provider) as Arc<dyn RateProvider>),
    );
    assert_eq!(restarted.fetch_due().await.unwrap(), 1);

    assert_eq!(lines_since(&start, "WARN", "DKK", FETCH_FAILED), 1);
    assert_eq!(lines_since(&start, "DEBUG", "DKK", FETCH_FAILED), 1);
}

#[tokio::test]
async fn a_different_failure_warns_again() {
    let (server, provider, fx, start) = logging("PLN").await;
    for (failure, warnings) in [
        (RateError::Network, 1),
        (RateError::Unavailable { status: 503 }, 2),
        (RateError::Unavailable { status: 502 }, 2),
        (RateError::Network, 3),
    ] {
        provider.answer("PLN", Err(failure));
        fx.fetch_due().await.unwrap();
        server.clock.advance(hours(1));
        assert_eq!(
            lines_since(&start, "WARN", "PLN", FETCH_FAILED),
            warnings,
            "{failure:?}"
        );
    }
}

/// Whoever saw the warning sees the end of it, once.
#[tokio::test]
async fn a_recovery_is_said_once() {
    let (server, provider, fx, start) = logging("HUF").await;
    provider.answer("HUF", Err(RateError::Network));
    fx.fetch_due().await.unwrap();

    provider.answer("HUF", Ok(vec![rate(FRIDAY, "400")]));
    for _ in 0..2 {
        server.clock.advance(hours(6));
        fx.fetch_due().await.unwrap();
    }

    assert_eq!(lines_since(&start, "WARN", "HUF", "again"), 1);
    assert_eq!(lines_since(&start, "WARN", "HUF", FETCH_FAILED), 1);
}

/// A currency the source doesn't have, whether it says so or answers with
/// nothing, is said once as well.
#[tokio::test]
async fn not_published_is_said_once() {
    for (code, answer) in [
        ("CZK", Err(RateError::NotPublished)),
        ("ISK", Ok(Vec::new())),
    ] {
        let (server, provider, fx, start) = logging(code).await;
        provider.answer(code, answer);
        for _ in 0..2 {
            assert_eq!(fx.fetch_due().await.unwrap(), 1);
            server.clock.advance(hours(24));
        }

        assert_eq!(
            lines_since(&start, "WARN", code, "not published"),
            1,
            "{code}"
        );
        assert_eq!(
            lines_since(&start, "DEBUG", code, "not published"),
            1,
            "{code}"
        );
    }
}

#[tokio::test]
async fn a_good_answer_says_nothing() {
    let (server, provider, fx, start) = logging("RON").await;
    provider.answer("RON", Ok(vec![rate(FRIDAY, "5")]));
    for _ in 0..2 {
        fx.fetch_due().await.unwrap();
        server.clock.advance(hours(6));
    }

    assert_eq!(lines_since(&start, "WARN", "RON", ""), 0);
    assert_eq!(lines_since(&start, "DEBUG", "RON", ""), 0);
}

/// An attempt whose answer never came, as after a crash, is on record as failing
/// with no code. Nothing was said about it, so it isn't a failure to repeat or to
/// recover from.
#[tokio::test]
async fn an_attempt_cut_short_is_not_a_failure_said_before() {
    for (code, answer, warnings) in [
        ("KRW", Err(RateError::Network), 1),
        ("THB", Ok(vec![rate(FRIDAY, "38")]), 0),
    ] {
        let (server, provider, fx, start) = logging(code).await;
        let mut tx = server.store.begin().await.unwrap();
        tx.record_fx_attempt(cur(code), server.clock.now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        provider.answer(code, answer);

        server.clock.advance(hours(1));
        assert_eq!(fx.fetch_due().await.unwrap(), 1);

        assert_eq!(lines_since(&start, "WARN", code, ""), warnings, "{code}");
    }
}
