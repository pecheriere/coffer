//! The outbound client against a loopback mock, in testkit mode (which also
//! accepts `http://127.0.0.1:<port>` for an allow-list built with
//! `HostAllowList::loopback`).

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use coffer_server::egress::{Egress, EgressError, HostAllowList, MAX_BODY_BYTES, OutboundRequest};
use reqwest::Method;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use url::Url;

/// Serves `router` on a loopback port and returns the port.
async fn serve(router: Router) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await });
    port
}

/// A loopback port with nothing listening on it.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);
    addr.port()
}

fn get_request(url: &str) -> OutboundRequest {
    OutboundRequest {
        method: Method::GET,
        url: Url::parse(url).unwrap(),
        basic_auth: None,
        headers: Vec::new(),
        body: None,
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let router = Router::new()
        .route(
            "/start",
            get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/target")]) }),
        )
        .route(
            "/target",
            get(move || async move {
                counter.fetch_add(1, Ordering::SeqCst);
                "reached"
            }),
        );
    let port = serve(router).await;
    let egress = Egress::for_tests();
    let allowed = HostAllowList::loopback(port);

    let response = egress
        .send(
            get_request(&format!("http://127.0.0.1:{port}/start")),
            &allowed,
        )
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::FOUND);
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect was followed");
}

#[tokio::test]
async fn oversized_body_refused() {
    let router = Router::new()
        // Declared length over the cap.
        .route(
            "/declared",
            get(|| async { vec![b'a'; MAX_BODY_BYTES + 1].into_response() }),
        )
        // Exactly at the cap is accepted.
        .route(
            "/limit",
            get(|| async { vec![b'a'; MAX_BODY_BYTES].into_response() }),
        );
    let port = serve(router).await;
    let egress = Egress::for_tests();
    let allowed = HostAllowList::loopback(port);

    let err = egress
        .send(
            get_request(&format!("http://127.0.0.1:{port}/declared")),
            &allowed,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::TooLarge), "{err:?}");

    // No declared length: the cap applies while reading.
    let chunked = serve_chunked(17).await;
    let err = egress
        .send(
            get_request(&format!("http://127.0.0.1:{chunked}/")),
            &HostAllowList::loopback(chunked),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::TooLarge), "{err:?}");
    let response = egress
        .send(
            get_request(&format!("http://127.0.0.1:{port}/limit")),
            &allowed,
        )
        .await
        .unwrap();
    assert_eq!(response.body.len(), MAX_BODY_BYTES);
}

/// A caller's smaller limit applies to a declared length and while reading
/// a body of no declared length.
#[tokio::test]
async fn caller_body_limit_applies_while_reading() {
    let router = Router::new()
        .route(
            "/declared",
            get(|| async { vec![b'a'; 4096].into_response() }),
        )
        .route("/limit", get(|| async { vec![b'a'; 2048].into_response() }));
    let port = serve(router).await;
    let egress = Egress::for_tests();
    let allowed = HostAllowList::loopback(port);
    let send = |path: &str, port: u16, allowed: HostAllowList| {
        let egress = egress.clone();
        let url = format!("http://127.0.0.1:{port}{path}");
        async move { egress.send_limited(get_request(&url), &allowed, 2048).await }
    };

    let declared = send("/declared", port, allowed.clone()).await;
    assert!(matches!(declared, Err(EgressError::TooLarge)));
    let chunked = serve_chunked(1).await;
    let streamed = send("/", chunked, HostAllowList::loopback(chunked)).await;
    assert!(matches!(streamed, Err(EgressError::TooLarge)));
    let at_limit = send("/limit", port, allowed).await.unwrap();
    assert_eq!(at_limit.body.len(), 2048);
}

/// Answers every connection with a chunked body (no declared length) of
/// `chunks` chunks of 1 MiB.
async fn serve_chunked(chunks: usize) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = vec![0; 4096];
                let _ = stream.read(&mut request).await;
                let chunk = vec![b'a'; 1024 * 1024];
                let mut out = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
                for _ in 0..chunks {
                    out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                    out.extend_from_slice(&chunk);
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(b"0\r\n\r\n");
                let _ = stream.write_all(&out).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn errors_do_not_leak_url_secrets() {
    let assert_clean = |err: &EgressError| {
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            let shown = format!("{cause} {cause:?}");
            assert!(!shown.contains("SECRET"), "{shown}");
            source = cause.source();
        }
    };

    // Refused before any connection.
    let egress = Egress::for_tests();
    let allowed = HostAllowList::parse("bridge.example").unwrap();
    let err = egress
        .send(
            get_request("https://other.example/claim/SECRETPART?token=SECRETQ"),
            &allowed,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::HostNotAllowed), "{err:?}");
    assert_clean(&err);

    // A connection that fails.
    let port = closed_port().await;
    let err = egress
        .send(
            get_request(&format!(
                "http://127.0.0.1:{port}/claim/SECRETPART?token=SECRETQ"
            )),
            &HostAllowList::loopback(port),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::Transport { .. }), "{err:?}");
    assert_clean(&err);

    // A dead proxy with credentials in its URL.
    let port = closed_port().await;
    let proxy = Url::parse(&format!("http://user:SECRETPW@127.0.0.1:{port}")).unwrap();
    let egress = Egress::new(Some(&proxy)).unwrap();
    let shown = format!("{egress:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    let err = egress
        .send(
            get_request("https://bridge.example/claim/SECRETPART?token=SECRETQ"),
            &allowed,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, EgressError::Transport { .. }), "{err:?}");
    assert_clean(&err);
}
