//! `serve`: what it logs at startup and how it stops, with a captured log.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_server::cli::{open_state, serve_with};
use coffer_server::config::Config;
use coffer_server::testkit::TestServer;
use secrecy::ExposeSecret as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument::WithSubscriber as _;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs the server on an ephemeral port, checks it answers, shuts it down
/// and returns everything it logged.
async fn run_and_stop(server: &TestServer) -> String {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(
        serve_with(server.app_state(), listener, async {
            let _ = stopped.await;
        })
        .with_subscriber(subscriber),
    );

    let mut stream = loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => break stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    };
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "GET /healthz");

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops after the shutdown future resolves")
        .unwrap()
        .unwrap();
    log.text()
}

#[tokio::test]
async fn serve_logs_the_setup_link_when_no_admin_exists_and_stops_cleanly() {
    let server = TestServer::new().await;

    let log = run_and_stop(&server).await;

    assert!(log.contains("https://coffer.test/setup/"), "{log}");
    assert!(log.contains("coffer stopped"), "{log}");
    assert!(log.contains("listen=127.0.0.1:"), "{log}");
    assert!(!log.contains("Some("), "{log}");
}

#[tokio::test]
async fn serve_does_not_log_a_setup_link_once_an_admin_exists() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    server.authenticator().enroll(&link, "alice", "Alice").await;

    let log = run_and_stop(&server).await;

    assert!(!log.contains("/setup/"), "{log}");
    assert!(log.contains("coffer is serving"), "{log}");
    assert!(log.contains("coffer stopped"), "{log}");
}

/// The real wiring (`open_state`, as `serve` builds it) registers
/// `SimpleFin` with the configured hosts only: a setup token for another
/// host is refused before anything is sent.
#[tokio::test]
async fn production_wiring_refuses_unlisted_setup_token() {
    let dir = tempfile::tempdir().unwrap();
    // `open_state` refuses a data directory others can read.
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
        _ => None,
    })
    .unwrap();
    let state = open_state(config).await.unwrap();
    let server = TestServer::open_at(dir.path()).await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, token) = state
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("planner").unwrap(),
            Scope::Write,
            None,
        )
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(serve_with(state, listener, async {
        let _ = stopped.await;
    }));
    let setup_token = STANDARD.encode("https://internal.example/claim/x");
    let body = format!(r#"{{"label":"Family bank","setup_token":"{setup_token}"}}"#);
    let request = format!(
        "POST /api/v1/connections/simplefin HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        token.expose_secret(),
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();

    assert!(reply.starts_with("HTTP/1.1 422"));
    assert!(reply.contains("\"code\":\"setup_token_invalid\""));
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops")
        .unwrap()
        .unwrap();
}

/// Sends one HTTP/1.0 request (so the reply is never chunked) and returns
/// the status code and body.
async fn http(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, String) {
    let headers = body.map_or_else(String::new, |body| {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    });
    let request = format!(
        "{method} {path} HTTP/1.0\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         {headers}\r\n{}",
        body.unwrap_or_default()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    let reply = String::from_utf8(reply).unwrap();
    let (head, body) = reply.split_once("\r\n\r\n").unwrap();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap();
    (status, body.to_owned())
}

/// The whole server, as `serve` runs it (scheduler included), on a real
/// listener: a write token creates a manual account and a transaction in
/// it, lists it back, and fetches the OpenAPI document; then it stops.
#[tokio::test]
async fn serve_creates_account_and_transaction_end_to_end() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("planner").unwrap(),
            Scope::Write,
            None,
        )
        .await
        .unwrap();
    let token = token.expose_secret().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(serve_with(server.app_state(), listener, async {
        let _ = stopped.await;
    }));

    let (status, account) = http(
        addr,
        "POST",
        "/api/v1/accounts",
        &token,
        Some(r#"{"name":"Cash","kind":"checking","currency":"EUR"}"#),
    )
    .await;
    assert_eq!(status, 201);
    let account: serde_json::Value = serde_json::from_str(&account).unwrap();
    let account_id = account["id"].as_str().unwrap().to_owned();

    let create = format!(
        r#"{{"account":"{account_id}","posted_on":"2027-01-10","minor_units":-1250,"description":"Groceries"}}"#
    );
    let (status, created) = http(addr, "POST", "/api/v1/transactions", &token, Some(&create)).await;
    assert_eq!(status, 201);
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();

    let (status, listed) = http(
        addr,
        "GET",
        &format!("/api/v1/transactions?account={account_id}"),
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], created["id"]);
    assert_eq!(items[0]["amount"]["minor_units"], serde_json::json!(-1250));

    let (status, document) = http(addr, "GET", "/api/v1/openapi.json", &token, None).await;
    assert_eq!(status, 200);
    let document: serde_json::Value = serde_json::from_str(&document).unwrap();
    assert!(document["paths"]["/api/v1/connections/{connection_id}/sync"].is_object());

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops")
        .unwrap()
        .unwrap();
}
