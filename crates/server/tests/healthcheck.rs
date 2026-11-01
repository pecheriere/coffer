//! `coffer healthcheck`: probes `GET /healthz` on `COFFER_LISTEN`, run
//! in-process with nothing but that variable set.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::process::ExitCode;

use coffer_server::cli::{self, serve_with};
use coffer_server::testkit::TestServer;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

struct Outcome {
    code: ExitCode,
    out: String,
    err: String,
}

/// Runs `coffer healthcheck` with `COFFER_LISTEN` as the whole environment:
/// no public URL, data directory or key, which the probe must not need.
async fn healthcheck(listen: &str) -> Outcome {
    let env = HashMap::from([("COFFER_LISTEN", listen.to_owned())]);
    let args = ["coffer", "healthcheck"].map(OsString::from);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::run(args, |name| env.get(name).cloned(), &mut out, &mut err).await;
    Outcome {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

/// Serves one connection on `listener` with `response`, after reading the
/// request head.
async fn answer_once(listener: TcpListener, response: &'static [u8]) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.ends_with(b"\r\n\r\n") {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0, "the probe closed before sending a full request");
        request.extend_from_slice(&buf[..n]);
    }
    assert!(request.starts_with(b"GET /healthz HTTP/1.1\r\n"));
    stream.write_all(response).await.unwrap();
}

#[tokio::test]
async fn healthy_server_on_the_unspecified_address_exits_0() {
    let server = TestServer::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(serve_with(server.app_state(), listener, async {
        let _ = stopped.await;
    }));

    // The server binds every interface; the probe goes to loopback.
    let probe = healthcheck(&format!("0.0.0.0:{port}")).await;

    assert_eq!(probe.code, ExitCode::SUCCESS);
    assert_eq!(probe.out, "");
    assert_eq!(probe.err, "");
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn unavailable_server_exits_1() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stub = tokio::spawn(answer_once(
        listener,
        b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 11\r\nconnection: close\r\n\r\nunavailable",
    ));

    let probe = healthcheck(&addr.to_string()).await;

    assert_eq!(probe.code, ExitCode::from(1));
    assert!(
        probe.err.contains("503"),
        "the probe error must mention the status"
    );
    stub.await.unwrap();
}

#[tokio::test]
async fn nothing_listening_exits_1() {
    let addr: SocketAddr = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };

    let probe = healthcheck(&addr.to_string()).await;

    assert_eq!(probe.code, ExitCode::from(1));
    assert!(
        probe.err.starts_with("coffer: "),
        "the probe error must start with the program name"
    );
}

#[tokio::test]
async fn an_invalid_listen_address_is_unhealthy_not_a_usage_error() {
    // Docker reserves exit code 2 in health checks, so every failure is 1.
    let probe = healthcheck("not-an-address").await;

    assert_eq!(probe.code, ExitCode::from(1));
    assert!(
        probe.err.contains("COFFER_LISTEN"),
        "the probe error must name COFFER_LISTEN"
    );
}
