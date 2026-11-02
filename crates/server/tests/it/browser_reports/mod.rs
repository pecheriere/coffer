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

use std::sync::atomic::{AtomicU32, Ordering};

use axum::body::Body;
use axum::http::{Method, header};
use coffer_server::assets::JS_COFFER_JS;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_server::testkit::{TEST_PUBLIC_URL, TestServer, captured_log_text};
use serde_json::{Value, json};

const PATH: &str = "/browser-reports";
const JSON: &str = "application/json";
const CSP: &str = "application/csp-report";

/// A body the endpoint would log, with `marker` as its message.
fn script_report(marker: &str) -> Value {
    json!({ "kind": "error", "message": marker, "script": JS_COFFER_JS.url, "page": "/login" })
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

/// A line number no other report in this process has. Tests may share a
/// process and so a log, and a CSP report's text is fixed: this tells a test's
/// own events from the rest.
fn fresh_line() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(100_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The logged events for `line`, joined by newlines; empty if there are none.
fn events_on(line: u32) -> String {
    let wanted = format!("line={line}");
    captured_log_text()
        .lines()
        .filter(|event| event.split_whitespace().any(|field| field == wanted))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Posts a script's report on a line of its own, and returns what it logged.
async fn script_events(browser: &mut Browser, body: &Value) -> String {
    let line = fresh_line();
    let mut body = body.clone();
    body["line"] = json!(line);
    post_script(browser, &body).await;
    events_on(line)
}

/// `body`, a CSP report, on a line of its own.
fn csp_on_fresh_line(body: &Value) -> (Value, u32) {
    let line = fresh_line();
    let mut body = body.clone();
    body["csp-report"]["line-number"] = json!(line);
    (body, line)
}

/// Posts a CSP report on a line of its own, and returns what it logged.
async fn csp_events(browser: &mut Browser, body: &Value) -> String {
    let (body, line) = csp_on_fresh_line(body);
    post_csp(browser, &body).await;
    events_on(line)
}

/// The logged lines holding `marker`. Markers are unique among these tests,
/// and none is the start of another.
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
