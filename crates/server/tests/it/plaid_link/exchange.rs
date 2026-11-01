//! The public token exchange: authorization, sealing and what is never echoed.

use super::{
    ACCESS_MARKER, ACCESS_TOKEN, EXCHANGE_URL, PUBLIC_MARKER, PUBLIC_TOKEN, PUBLIC_TOKEN_EXCHANGE,
    World, contains, world,
};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::sync::SyncTrigger;
use coffer_server::testkit::browser::Browser;
use coffer_server::testkit::{TEST_PUBLIC_URL, captured_log_text, log_marker};
use coffer_store::ConnectionStatus;
use secrecy::ExposeSecret as _;
use serde_json::json;

#[tokio::test]
async fn exchange_requires_session_and_csrf() {
    let mut world = world().await;
    let body = json!({ "label": "Joint", "public_token": PUBLIC_TOKEN });
    let csrf = World::csrf(&mut world.admin).await;

    // A bearer token authenticates no page endpoint.
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("agent").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut anonymous = Browser::new(&world.server);
    let bearer = Request::builder()
        .method(Method::POST)
        .uri(EXCHANGE_URL)
        .header(header::ORIGIN, TEST_PUBLIC_URL)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        anonymous.send(bearer).await.status,
        StatusCode::UNAUTHORIZED
    );

    let missing = world
        .admin
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), None, &body)
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let wrong = world
        .admin
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), Some("wrong"), &body)
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let cross = world
        .admin
        .post_json_with(
            EXCHANGE_URL,
            Some("https://evil.example"),
            Some(&csrf),
            &body,
        )
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    let no_origin = world
        .admin
        .post_json_with(EXCHANGE_URL, None, Some(&csrf), &body)
        .await;
    assert_eq!(no_origin.status, StatusCode::FORBIDDEN);

    // Authentication comes first: a body it cannot read says nothing to a
    // request without a session.
    let unreadable = anonymous
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), None, &json!(7))
        .await;
    assert_eq!(unreadable.status, StatusCode::UNAUTHORIZED);

    assert_eq!(
        world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(),
        0,
        "nothing reached Plaid"
    );
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn exchange_stores_sealed_access_token() {
    let mut world = world().await;
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::OK,
        json!({ "access_token": ACCESS_TOKEN, "item_id": "item-1" }).to_string(),
    );
    let marker = log_marker();

    let reply = world
        .exchange(&json!({ "label": "Joint", "public_token": PUBLIC_TOKEN }))
        .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "redirect": "/connections" }));
    let seen = world.script.seen(PUBLIC_TOKEN_EXCHANGE);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["public_token"], PUBLIC_TOKEN);

    let connections = world.server.store.list_connections().await.unwrap();
    assert_eq!(connections.len(), 1);
    let connection = &connections[0];
    assert_eq!(connection.provider, "plaid");
    assert_eq!(connection.label.as_str(), "Joint");
    assert_eq!(connection.owner, world.alice.member);
    assert_eq!(connection.status, ConnectionStatus::Active);
    let opened = world
        .server
        .vault
        .open(
            connection.id,
            &connection.credentials.clone().unwrap().into(),
        )
        .unwrap();
    assert!(
        opened.expose_secret().contains(ACCESS_MARKER),
        "positive control: the sealed bytes hold the token"
    );

    for marker in [ACCESS_MARKER, PUBLIC_MARKER] {
        assert!(
            !contains(&world.data_dir_bytes(), marker),
            "no token in the data directory"
        );
        assert!(!world.audit_text().await.contains(marker), "nor the audit");
        assert!(!reply.body.contains(marker), "nor the response");
        assert!(!captured_log_text().contains(marker), "nor the log");
    }
    assert!(captured_log_text().contains(&marker));

    let runs = world.runs(connection.id).await;
    assert_eq!(runs.len(), 1, "a sync is queued");
    assert_eq!(runs[0].trigger, SyncTrigger::Initial);
}

#[tokio::test]
async fn public_token_never_echoed_on_error() {
    let mut world = world().await;
    let marker = log_marker();
    let body = json!({ "label": "Joint", "public_token": PUBLIC_TOKEN });
    let echoing = json!({
        "error_type": "INVALID_INPUT",
        "error_code": "INVALID_PUBLIC_TOKEN",
        "error_message": format!("the token {PUBLIC_TOKEN} is no good"),
        "display_message": PUBLIC_TOKEN,
    });
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::BAD_REQUEST,
        echoing.to_string(),
    );
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::BAD_GATEWAY,
        format!("not json {PUBLIC_TOKEN}"),
    );
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::OK,
        format!("{{\"unexpected\":\"{PUBLIC_TOKEN}\"}}"),
    );

    for status in [
        StatusCode::BAD_GATEWAY,
        StatusCode::BAD_GATEWAY,
        StatusCode::BAD_GATEWAY,
    ] {
        let reply = world.exchange(&body).await;
        assert_eq!(reply.status, status);
        assert!(!reply.body.contains(PUBLIC_MARKER), "a Plaid failure");
    }
    assert_eq!(world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(), 3);

    // Refused by coffer, with no request to Plaid.
    for refused in [
        json!({ "label": "", "public_token": PUBLIC_TOKEN }),
        json!({ "label": "a\u{7}b", "public_token": PUBLIC_TOKEN }),
        json!({ "label": "x".repeat(101), "public_token": PUBLIC_TOKEN }),
        json!({ "label": "Joint", "public_token": "   " }),
        json!({ "public_token": PUBLIC_TOKEN }),
        json!({ "label": 7, "public_token": PUBLIC_TOKEN }),
        json!({ "label": "Joint", "public_token": PUBLIC_TOKEN, "extra": PUBLIC_TOKEN }),
    ] {
        let reply = world.exchange(&refused).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!reply.body.contains(PUBLIC_MARKER), "a refusal");
    }
    assert_eq!(world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(), 3);
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        0
    );

    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    assert!(!log.contains(PUBLIC_MARKER), "the token must not be logged");
    assert!(!world.audit_text().await.contains(PUBLIC_MARKER));
}
