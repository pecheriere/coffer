//! The limits on what can reach the log. These tests use small counts and the
//! test clock.

use axum::http::header;
use coffer_server::testkit::browser::Browser;
use coffer_server::testkit::{TEST_PUBLIC_URL, log_marker};
use jiff::SignedDuration;
use serde_json::json;

use super::{CSP, JSON, browser, csp_report, lines_with, log_since, post, script_report};

const PER_CLIENT: usize = 5;
const GLOBAL: usize = 20;
const SUMMARY: &str = "browser reports dropped";

async fn from_client(browser: &mut Browser, client: &str, marker: &str) {
    let headers = [
        (header::CONTENT_TYPE, JSON),
        (header::ORIGIN, TEST_PUBLIC_URL),
        (header::HeaderName::from_static("x-forwarded-for"), client),
    ];
    post(browser, &headers, script_report(marker).to_string()).await;
}

#[tokio::test]
async fn a_client_past_its_limit_is_not_logged() {
    let (_server, mut browser) = browser().await;
    for _ in 0..PER_CLIENT + 3 {
        from_client(&mut browser, "100.64.0.1", "rate-one-client").await;
    }
    assert_eq!(lines_with("rate-one-client").len(), PER_CLIENT);
}

#[tokio::test]
async fn another_client_has_its_own_limit() {
    let (_server, mut browser) = browser().await;
    for _ in 0..=PER_CLIENT {
        from_client(&mut browser, "100.64.0.1", "rate-first").await;
    }
    from_client(&mut browser, "100.64.0.2", "rate-second").await;
    assert_eq!(lines_with("rate-second").len(), 1);
}

/// A proxy appends the address it saw, so only the last entry can be trusted:
/// a client choosing the entries before it does not escape its limit.
#[tokio::test]
async fn a_client_cannot_escape_its_limit_with_a_forged_forwarded_entry() {
    let (_server, mut browser) = browser().await;
    for forged in 1..=PER_CLIENT + 3 {
        let client = format!("10.0.0.{forged}, 100.64.0.1");
        from_client(&mut browser, &client, "rate-forged").await;
    }
    assert_eq!(lines_with("rate-forged").len(), PER_CLIENT);
}

#[tokio::test]
async fn clients_with_no_usable_address_share_one_limit() {
    let (_server, mut browser) = browser().await;
    for rotating in 0..PER_CLIENT + 3 {
        let client = format!("not-an-address-{rotating}");
        from_client(&mut browser, &client, "rate-unknown").await;
    }
    assert_eq!(lines_with("rate-unknown").len(), PER_CLIENT);
}

/// What holds if a client can choose its address, as it can with no proxy in
/// front of coffer.
#[tokio::test]
async fn many_clients_together_stay_within_the_global_bound() {
    let (_server, mut browser) = browser().await;
    for client in 1..=GLOBAL + 5 {
        from_client(&mut browser, &format!("100.64.1.{client}"), "rate-many").await;
    }
    assert_eq!(lines_with("rate-many").len(), GLOBAL);
}

#[tokio::test]
async fn the_limit_starts_again_after_a_minute() {
    let (server, mut browser) = browser().await;
    for _ in 0..=PER_CLIENT {
        from_client(&mut browser, "100.64.0.1", "rate-before").await;
    }
    server.clock.advance(SignedDuration::from_mins(1));
    from_client(&mut browser, "100.64.0.1", "rate-after").await;
    assert_eq!(lines_with("rate-after").len(), 1);
}

#[tokio::test]
async fn drops_are_not_logged_one_by_one() {
    let (_server, mut browser) = browser().await;
    let start = log_marker();
    for _ in 0..PER_CLIENT + 7 {
        from_client(&mut browser, "100.64.0.1", "rate-quiet").await;
    }
    assert!(!log_since(&start).contains(SUMMARY));
}

#[tokio::test]
async fn the_count_of_drops_is_logged_after_five_minutes() {
    let (server, mut browser) = browser().await;
    let start = log_marker();
    for _ in 0..PER_CLIENT + 7 {
        from_client(&mut browser, "100.64.0.1", "rate-counted").await;
    }
    server.clock.advance(SignedDuration::from_mins(5));
    from_client(&mut browser, "100.64.0.1", "rate-counted").await;
    let log = log_since(&start);
    let summaries: Vec<&str> = log.lines().filter(|line| line.contains(SUMMARY)).collect();
    assert_eq!(summaries.len(), 1, "{summaries:?}");
    assert!(summaries[0].contains("over_limit=7"), "{}", summaries[0]);
}

#[tokio::test]
async fn the_count_of_drops_is_logged_at_most_once_per_period() {
    let (server, mut browser) = browser().await;
    let start = log_marker();
    for _ in 0..=PER_CLIENT {
        from_client(&mut browser, "100.64.0.1", "rate-period").await;
    }
    server.clock.advance(SignedDuration::from_mins(5));
    for _ in 0..PER_CLIENT + 3 {
        from_client(&mut browser, "100.64.0.1", "rate-period").await;
    }
    server.clock.advance(SignedDuration::from_mins(1));
    for _ in 0..PER_CLIENT + 3 {
        from_client(&mut browser, "100.64.0.1", "rate-period").await;
    }
    let log = log_since(&start);
    assert_eq!(log.lines().filter(|line| line.contains(SUMMARY)).count(), 1);
}

#[tokio::test]
async fn refused_reports_are_counted_apart() {
    let (server, mut browser) = browser().await;
    let start = log_marker();
    post(&mut browser, &[(header::CONTENT_TYPE, "text/plain")], "x").await;
    post(&mut browser, &[(header::CONTENT_TYPE, "text/plain")], "x").await;
    server.clock.advance(SignedDuration::from_mins(5));
    post(&mut browser, &[(header::CONTENT_TYPE, "text/plain")], "x").await;
    let log = log_since(&start);
    let summary = log.lines().find(|line| line.contains(SUMMARY)).unwrap();
    assert!(summary.contains("refused=2"), "{summary}");
}

/// A proxy may add its entry as a header line of its own: the last line
/// counts, not the first one, which a client chose.
#[tokio::test]
async fn a_client_cannot_escape_its_limit_with_a_forged_forwarded_line() {
    let (_server, mut browser) = browser().await;
    let forwarded = header::HeaderName::from_static("x-forwarded-for");
    for forged in 1..=PER_CLIENT + 3 {
        let forged = format!("10.0.0.{forged}");
        let headers = [
            (header::CONTENT_TYPE, JSON),
            (header::ORIGIN, TEST_PUBLIC_URL),
            (forwarded.clone(), forged.as_str()),
            (forwarded.clone(), "100.64.0.1"),
        ];
        post(
            &mut browser,
            &headers,
            script_report("rate-lines").to_string(),
        )
        .await;
    }
    assert_eq!(lines_with("rate-lines").len(), PER_CLIENT);
}

/// Otherwise another site's page, or anyone sending junk, could silence a real
/// browser.
#[tokio::test]
async fn refused_requests_spend_no_budget() {
    let (_server, mut browser) = browser().await;
    let forwarded = header::HeaderName::from_static("x-forwarded-for");
    let client = (forwarded, "100.64.0.1");
    let page = (header::ORIGIN, TEST_PUBLIC_URL);
    let script = (header::CONTENT_TYPE, JSON);
    let csp = (header::CONTENT_TYPE, CSP);
    let mut unknown = script_report("rate-refused");
    unknown["stack"] = json!("at somewhere");
    let mut foreign = csp_report("/login", "img-src", "inline");
    foreign["csp-report"]["document-uri"] = json!("https://evil.test/login");
    for _ in 0..=GLOBAL {
        let good = script_report("rate-refused").to_string();
        let as_page = [script.clone(), page.clone(), client.clone()];
        let bare = [script.clone(), client.clone()];
        let plain = [(header::CONTENT_TYPE, "text/plain"), client.clone()];
        let from_csp = [csp.clone(), client.clone()];
        post(&mut browser, &as_page, "{not json").await;
        post(&mut browser, &as_page, unknown.to_string()).await;
        post(&mut browser, &as_page, "x".repeat(10_000)).await;
        post(&mut browser, &bare, good.clone()).await;
        post(&mut browser, &plain, good).await;
        post(&mut browser, &from_csp, foreign.to_string()).await;
    }
    from_client(&mut browser, "100.64.0.1", "rate-still-heard").await;
    assert_eq!(lines_with("rate-still-heard").len(), 1);
}

/// Every Link page causes it, so it must not use up the page's own reports.
#[tokio::test]
async fn plaid_s_expected_violation_spends_no_budget() {
    let (_server, mut browser) = browser().await;
    let forwarded = header::HeaderName::from_static("x-forwarded-for");
    let headers = [(header::CONTENT_TYPE, CSP), (forwarded, "100.64.0.1")];
    let expected = csp_report("/connections/plaid/new", "style-src-elem", "inline").to_string();
    for _ in 0..=PER_CLIENT {
        post(&mut browser, &headers, expected.clone()).await;
    }
    from_client(&mut browser, "100.64.0.1", "rate-after-plaid").await;
    assert_eq!(lines_with("rate-after-plaid").len(), 1);
}
