//! Connecting and reconnecting through the API.

use super::{
    ACCOUNTS_FIRST, BOGUS_TOKEN, PASSWORD_ENCODED, PASSWORD_MARKER, Reply, USER, contains,
    setup_token, world,
};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::identity::{Scope, TokenName};
use coffer_server::http::router;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use coffer_store::ConnectionStatus;
use http_body_util::BodyExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
use tower::ServiceExt as _;

#[tokio::test]
async fn reconnect_with_new_setup_token() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));
    world.sync(connection).await;
    let checking = world.bind("ACT-0001", AccountKind::Checking).await;
    world
        .script
        .bridge_reply(Reply::status(StatusCode::FORBIDDEN, ""));
    world.sync(connection).await;
    assert_eq!(
        world
            .server
            .store
            .get_connection(connection)
            .await
            .unwrap()
            .status,
        ConnectionStatus::ReauthRequired
    );

    let renewed = format!("http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin");
    world.script.claim(Reply::ok(&renewed));
    let (status, raw, view) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{connection}/simplefin/reconnect"),
            &json!({ "setup_token": setup_token(&world.claim_url()) }),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["status"], json!("active"));
    assert!(!contains(&raw, PASSWORD_MARKER));
    let bound = world.provider_account("ACT-0001").await;
    assert_eq!(bound.account, Some(checking));
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::Active);
    assert!(world.opens_to(connection, record.credentials.unwrap(), &world.access_url()));
}

#[tokio::test]
async fn reconnect_with_invalid_token_changes_nothing() {
    let world = world().await;
    let connection = world.seeded().await;
    let before = world.server.store.get_connection(connection).await.unwrap();

    let (status, raw, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{connection}/simplefin/reconnect"),
            &json!({ "setup_token": BOGUS_TOKEN }),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("setup_token_invalid"));
    assert!(!contains(&raw, "SECRETTOKEN"));
    let after = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(after.credentials, before.credentials);
}

/// A read token cannot connect, and nothing reaches the bridge.
#[tokio::test]
async fn connect_needs_manage_connections() {
    let mut world = world().await;
    let (_, reader) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    world.writer = reader;

    let (status, _, problem) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("forbidden"));
    assert_eq!(world.mock.requests().len(), 0);
}

/// A retry with the same `Idempotency-Key` replays the first response and
/// claims nothing again.
#[tokio::test]
async fn connect_with_idempotency_key_replays() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin"
    )));
    let body = json!({ "label": "Family bank", "setup_token": setup_token(&world.claim_url()) });
    let send = || async {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/connections/simplefin")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", world.writer.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .header("Idempotency-Key", "connect-1")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(world.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes)
    };

    let (first_status, first) = send().await;
    let (second_status, second) = send().await;

    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second);
    assert_eq!(world.connection_count().await, 1);
    assert_eq!(world.mock.requests().len(), 1);
}

/// The provider registered by the configuration, connected directly:
/// the label is the caller's, the credentials the access URL.
#[tokio::test]
async fn service_connect_returns_the_access_url() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin"
    )));

    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse("Joint").unwrap(),
            ConnectInput::SimpleFinSetupToken(SecretString::from(setup_token(&world.claim_url()))),
            None,
        )
        .await
        .unwrap();

    assert_eq!(view.label, "Joint");
    let sealed = world
        .server
        .store
        .get_connection(view.id)
        .await
        .unwrap()
        .credentials
        .unwrap();
    assert!(world.opens_to(view.id, sealed, &world.access_url()));
}
