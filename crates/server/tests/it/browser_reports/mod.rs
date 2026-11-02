//! The endpoint browsers report errors to: what it accepts, what it logs and
//! what it answers. Everything here runs without signing in.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod answers;
mod csp;
mod fields;
mod limits;
mod log_formats;
mod rates;

use axum::body::Body;
use axum::http::{Method, header};
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_server::testkit::{TEST_PUBLIC_URL, TestServer, captured_log_text};
use serde_json::{Value, json};

const PATH: &str = "/browser-reports";
const JSON: &str = "application/json";
const CSP: &str = "application/csp-report";

/// A body the endpoint would log, with `marker` as its message.
fn script_report(marker: &str) -> Value {
    json!({ "kind": "error", "message": marker, "page": "/login" })
}

/// A browser's CSP report for a violation on `path`.
fn csp_report(path: &str, directive: &str, blocked: &str) -> Value {
    json!({ "csp-report": {
        "document-uri": format!("{TEST_PUBLIC_URL}{path}"),
        "effective-directive": directive,
        "violated-directive": directive,
        "blocked-uri": blocked,
    }})
}

async fn browser() -> (TestServer, Browser) {
    let server = TestServer::new().await;
    let browser = Browser::new(&server);
    (server, browser)
}

/// Posts `body` with exactly these headers.
async fn post(
    browser: &mut Browser,
    headers: &[(header::HeaderName, &str)],
    body: impl Into<Body>,
) -> Reply {
    let mut request = browser.builder(Method::POST, PATH);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    browser.send(request.body(body.into()).unwrap()).await
}

/// Posts a first-party script's report from the public origin.
async fn post_script(browser: &mut Browser, body: &Value) -> Reply {
    let headers = [
        (header::CONTENT_TYPE, JSON),
        (header::ORIGIN, TEST_PUBLIC_URL),
    ];
    post(browser, &headers, body.to_string()).await
}

/// Posts a CSP report as a browser does, with no `Origin`.
async fn post_csp(browser: &mut Browser, body: &Value) -> Reply {
    post(browser, &[(header::CONTENT_TYPE, CSP)], body.to_string()).await
}

/// The logged lines holding `marker`.
fn lines_with(marker: &str) -> Vec<String> {
    captured_log_text()
        .lines()
        .filter(|line| line.contains(marker))
        .map(str::to_owned)
        .collect()
}

/// The one report event holding `marker`.
fn the_event(marker: &str) -> String {
    let mut lines = lines_with(marker);
    assert_eq!(lines.len(), 1, "one event: {lines:?}");
    lines.remove(0)
}

/// The captured log since `start` (from `log_marker`), which must still be
/// held, so that something missing from it means something.
fn log_since(start: &str) -> String {
    let log = captured_log_text();
    let (_, since) = log.split_once(start).expect("the marker is still captured");
    since.to_owned()
}
