//! Accounts, balance snapshots and idempotent creates, through the real
//! router with agent tokens.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod idempotency;
mod management;
mod refusals;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::AgentTokenId;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    World {
        server,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

async fn token(world: &World, scope: Scope) -> (AgentTokenId, SecretToken) {
    let (record, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            scope,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    (record.id, secret)
}

fn request(
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<&str>,
    key: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri).header(
        header::AUTHORIZATION,
        format!("Bearer {}", token.expose_secret()),
    );
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

async fn send(server: &TestServer, request: Request<Body>) -> (StatusCode, Value) {
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

async fn call(
    server: &TestServer,
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map(|body| body.to_string());
    send(server, request(method, uri, token, body.as_deref(), None)).await
}

async fn create(server: &TestServer, token: &SecretToken, body: Value) -> Value {
    let (status, created) = call(server, Method::POST, "/api/v1/accounts", token, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED);
    created
}

async fn account_count(server: &TestServer) -> usize {
    server
        .store
        .list_accounts(&coffer_domain::policy::AccountScope::All, true)
        .await
        .unwrap()
        .len()
}

async fn audit_actions(server: &TestServer, action: &str) -> Vec<coffer_store::AuditEvent> {
    server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("account".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|event| event.action == action)
        .collect()
}
