//! What a panic does: the answer a client gets from a panicking handler, and
//! what the real binary reports. Each binary run is its own process, because
//! the panic hook is process-wide: installed here, it would swallow the
//! failure messages of every test running beside these.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::net::SocketAddr;
use std::process::{Command, Output};

use axum::Router;
use axum::routing::get;
use coffer_server::http::{AppState, router_with_test_routes};
use coffer_server::testkit::{PANIC_MARKER, TestServer, captured_log_text, log_marker};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const API_PANIC: &str = "/api/v1/test-panic";
const PAGE_PANIC: &str = "/test-panic";

/// A panic, as a handler bug would raise it.
async fn panicking_handler() -> &'static str {
    tokio::task::yield_now().await;
    panic!("{PANIC_MARKER}")
}

/// The real router and a real listener, with a panicking API and page route
/// that exist only in this test. The server lives as long as the result.
async fn serve() -> (TestServer, SocketAddr) {
    let server = TestServer::new().await;
    let panicking: Router<AppState> = Router::new()
        .route(API_PANIC, get(panicking_handler))
        .route(PAGE_PANIC, get(panicking_handler));
    let app = router_with_test_routes(server.app_state(), panicking);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (server, addr)
}

struct Answer {
    status: u16,
    /// The header block, lower-cased, one `name: value` to a line.
    headers: String,
    body: String,
}

impl Answer {
    fn has_header(&self, line: &str) -> bool {
        self.headers.lines().any(|header| header == line)
    }
}

/// `GET path` over a new connection. A dropped connection fails the test.
async fn get_path(addr: SocketAddr, path: &str) -> Answer {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    let (head, body) = reply
        .split_once("\r\n\r\n")
        .expect("the server answers instead of dropping the connection");
    let (status_line, headers) = head.split_once("\r\n").unwrap();
    Answer {
        status: status_line.split(' ').nth(1).unwrap().parse().unwrap(),
        headers: headers.replace("\r\n", "\n").to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

#[tokio::test]
async fn api_panic_answers_the_generic_problem() {
    let (_server, addr) = serve().await;
    let answer = get_path(addr, API_PANIC).await;
    assert_eq!(answer.status, 500);
    assert!(answer.has_header("content-type: application/problem+json"));
    assert_eq!(
        serde_json::from_str::<Value>(&answer.body).unwrap(),
        json!({
            "type": "urn:coffer:problem:internal",
            "title": "Internal error",
            "status": 500,
            "code": "internal",
        })
    );
}

#[tokio::test]
async fn page_panic_answers_a_generic_page() {
    let (_server, addr) = serve().await;
    let answer = get_path(addr, PAGE_PANIC).await;
    assert_eq!(answer.status, 500);
    assert!(answer.has_header("content-type: text/html; charset=utf-8"));
    assert!(answer.body.contains("Something went wrong"));
    assert!(!answer.body.contains(PANIC_MARKER));
}

#[tokio::test]
async fn panic_answers_carry_the_security_headers() {
    let (_server, addr) = serve().await;
    for path in [API_PANIC, PAGE_PANIC] {
        let answer = get_path(addr, path).await;
        assert_eq!(answer.status, 500);
        assert!(
            answer
                .headers
                .contains("content-security-policy: default-src 'self'"),
            "{path}"
        );
        assert!(answer.has_header("referrer-policy: same-origin"), "{path}");
        assert!(
            answer.has_header("x-content-type-options: nosniff"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn page_panic_answer_is_not_cached() {
    let (_server, addr) = serve().await;
    let answer = get_path(addr, PAGE_PANIC).await;
    assert!(answer.has_header("cache-control: no-store"));
}

#[tokio::test]
async fn server_keeps_serving_after_a_panic() {
    let (_server, addr) = serve().await;
    assert_eq!(get_path(addr, API_PANIC).await.status, 500);
    assert_eq!(get_path(addr, "/healthz").await.status, 200);
    assert_eq!(get_path(addr, API_PANIC).await.status, 500);
}

/// `CatchPanicLayer`'s default response logs the message; ours must not.
#[tokio::test]
async fn catching_a_panic_logs_nothing_of_its_message() {
    let (_server, addr) = serve().await;
    let start = log_marker();
    assert_eq!(get_path(addr, API_PANIC).await.status, 500);
    let log = captured_log_text();
    let (_, since_start) = log.split_once(&start).unwrap();
    assert!(!since_start.contains(PANIC_MARKER));
}

/// Runs the real binary's panic command with only `env` set, so no ambient
/// `RUST_BACKTRACE` or `RUST_LOG` decides the outcome.
fn panicking_binary(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_coffer"))
        .arg("panic-for-tests")
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// What a panic reports when logging as JSON, parsed: it must be exactly one
/// JSON line.
fn json_event() -> Value {
    let output = panicking_binary(&["--logging"], &[("COFFER_LOG_FORMAT", "json")]);
    let stderr = stderr_of(&output);
    let mut lines = stderr.lines();
    let event = serde_json::from_str(lines.next().expect("an event")).expect("a JSON line");
    assert_eq!(lines.next(), None, "one event: {stderr}");
    event
}

#[test]
fn panic_logs_one_json_error_event() {
    let event = json_event();
    assert_eq!(event["level"], json!("ERROR"));
    assert_eq!(event["fields"]["message"], json!("coffer panicked"));
}

#[test]
fn panic_event_names_the_location() {
    let event = json_event();
    let location = event["fields"]["location"].as_str().unwrap();
    assert!(location.contains("testkit/mod.rs:"), "{location}");
}

#[test]
fn panic_event_carries_a_backtrace_naming_file_and_function() {
    // No RUST_BACKTRACE in the environment: it is captured regardless.
    let event = json_event();
    let backtrace = event["fields"]["backtrace"].as_str().unwrap();
    assert!(backtrace.contains("panic_with_marker"), "{backtrace}");
    assert!(backtrace.contains("testkit/mod.rs:"), "{backtrace}");
}

#[test]
fn panic_event_leaves_out_the_message() {
    let output = panicking_binary(&["--logging"], &[("COFFER_LOG_FORMAT", "json")]);
    assert!(!stderr_of(&output).contains(PANIC_MARKER));
}

#[test]
fn panic_event_follows_the_text_format_by_default() {
    let output = panicking_binary(&["--logging"], &[]);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("ERROR"), "{stderr}");
    assert!(stderr.contains("coffer panicked"), "{stderr}");
    assert!(!stderr.starts_with('{'), "{stderr}");
    assert!(!stderr.contains(PANIC_MARKER));
}

/// No subscriber: a command such as `backup`, which sets up no logging, or a
/// panic before logging is set up.
#[test]
fn panic_without_a_subscriber_reports_on_stderr() {
    let output = panicking_binary(&[], &[]);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("testkit/mod.rs:"), "{stderr}");
    assert!(stderr.contains("panic_with_marker"), "{stderr}");
    assert!(!stderr.contains(PANIC_MARKER));
}

/// `RUST_LOG=off` silences logs, but a panic still says where it happened.
#[test]
fn panic_with_logs_filtered_out_reports_on_stderr() {
    let output = panicking_binary(&["--logging"], &[("RUST_LOG", "off")]);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("testkit/mod.rs:"), "{stderr}");
    assert!(!stderr.contains(PANIC_MARKER));
}

#[test]
fn panicking_command_exits_non_zero() {
    let output = panicking_binary(&[], &[]);
    assert_eq!(output.status.code(), Some(101));
}
