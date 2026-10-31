//! `serve`: what it logs at startup and how it stops, with a captured log.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use coffer_server::cli::serve_with;
use coffer_server::testkit::TestServer;
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
