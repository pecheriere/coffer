//! The MCP endpoint through the real router: the protocol at its current revision and an
//! older one, who gets in, what each tool looks like, and that a tool answers what its API
//! operation answers.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod emergency_fund;
mod parity;
mod transport;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

pub(crate) const PATH: &str = "/mcp";
/// Every real request has one, and the transport refuses one without.
pub(crate) const HOST: &str = "coffer.test";
/// The revision of most clients in use, and the last with an `initialize`.
pub(crate) const OLDER_REVISION: &str = "2025-11-25";
/// The protocol's current revision.
pub(crate) const CURRENT_REVISION: &str = "2026-07-28";

/// What came back over HTTP.
pub(crate) struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap()
    }
}

/// The headers of a client at [`OLDER_REVISION`] with `token`.
pub(crate) fn request_to_mcp(token: &SecretToken) -> axum::http::request::Builder {
    Request::builder()
        .method(Method::POST)
        .uri(PATH)
        .header(header::HOST, HOST)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", OLDER_REVISION)
}

pub(crate) async fn send(server: &TestServer, request: Request<Body>) -> Reply {
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        text: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// A JSON-RPC request, as `token`'s client sends it.
pub(crate) async fn rpc(
    server: &TestServer,
    token: &SecretToken,
    method: &str,
    params: Value,
) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply = send(
        server,
        request_to_mcp(token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{method}");
    reply.json()
}

/// What a tool answered: the value, or the problem of a refusal.
#[derive(Debug, PartialEq)]
pub(crate) enum Answer {
    Value(Value),
    Problem(Value),
}

/// Calls a tool. A value is also there as text, for a client that reads only that, and it
/// must be the same value.
pub(crate) async fn tool(
    server: &TestServer,
    token: &SecretToken,
    name: &str,
    arguments: Value,
) -> Answer {
    let answer = rpc(
        server,
        token,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    let result = &answer["result"];
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    if result["isError"] == true {
        assert!(result.get("structuredContent").is_none());
        Answer::Problem(text)
    } else {
        assert_eq!(result["structuredContent"], text);
        Answer::Value(text)
    }
}

/// What the API answers, put as [`tool`] puts it.
pub(crate) fn as_answer(status: StatusCode, body: Value) -> Answer {
    if status.is_success() {
        Answer::Value(body)
    } else {
        Answer::Problem(body)
    }
}
