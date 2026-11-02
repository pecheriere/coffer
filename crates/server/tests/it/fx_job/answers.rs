//! What an answer does to what is stored, and how a failure is recorded.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use coffer_domain::fx::{FxErrorCode, FxFetchStatus};
use coffer_server::fx::RateError;
use coffer_server::testkit::{MockUpstream, TestServer, captured_log_text, log_marker};
use jiff::SignedDuration;
use jiff::civil::date;

use super::{
    FRIDAY, MONDAY, SATURDAY, SUNDAY, coverage_of, display_minor, ecb_at, holding, hours, noon,
    rate, rates_of, rows_of, scripted, service, show_in, usd_holding_in_euros,
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
