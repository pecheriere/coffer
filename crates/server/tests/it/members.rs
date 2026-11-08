//! `Ledger::list_members`, the one read of the household's members that the API, its MCP tool
//! and the pages share.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_server::http::router;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

#[tokio::test]
async fn a_read_token_lists_the_members_the_api_answers() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let alice_id = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member: alice_id,
        role: Role::Admin,
        via: Via::Session,
    };
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let bob = server.authenticator().enroll(&invite, "bob", "Bob").await;
    server.identity.disable_member(&alice, bob).await.unwrap();
    let (_, secret) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let reader = server
        .agent_tokens
        .resolve(secret.expose_secret())
        .await
        .unwrap();

    let listed = server.ledger.list_members(&reader).await.unwrap();

    let request = Request::get("/api/v1/members")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", secret.expose_secret()),
        )
        .body(Body::empty())
        .unwrap();
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let answered: Value = serde_json::from_slice(&bytes).unwrap();
    let expected: Vec<Value> = listed
        .iter()
        .map(|member| {
            json!({
                "id": member.id,
                "username": member.username.as_str(),
                "display_name": member.display_name.as_str(),
                "role": member.role,
                "disabled_at": member.disabled_at,
            })
        })
        .collect();
    assert_eq!(answered["items"], Value::Array(expected));
    assert_eq!(listed.len(), 2, "disabled members included");
    assert!(
        listed
            .iter()
            .any(|member| member.id == bob && member.disabled_at.is_some())
    );
}
