//! Idempotent account creates.

use super::{account_count, audit_actions, request, send, token, world};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::Scope;
use coffer_server::http::router;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tower::ServiceExt as _;

const NEW_ACCOUNT: &str = r#"{"name": "Savings", "kind": "savings", "currency": "USD"}"#;

#[tokio::test]
async fn idempotent_retry_replays_response() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let post = || {
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("retry-1"),
        )
    };

    let (first_status, first) = send(&world.server, post()).await;
    let (second_status, second) = send(&world.server, post()).await;

    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first["id"], second["id"]);
    assert_eq!(first, second);
    assert_eq!(account_count(&world.server).await, 1);
    assert_eq!(
        audit_actions(&world.server, "account.create").await.len(),
        1
    );

    // Without a key, every request creates.
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(account_count(&world.server).await, 2);
}

#[tokio::test]
async fn idempotency_key_reuse_with_other_body_refused() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("reuse-1"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(r#"{"name": "Other", "kind": "savings", "currency": "USD"}"#),
            Some("reuse-1"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "idempotency_key_reuse");
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn failed_create_frees_its_idempotency_key() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let invalid = r#"{"name": "", "kind": "savings", "currency": "USD"}"#;
    for _ in 0..2 {
        let (status, body) = send(
            &world.server,
            request(
                Method::POST,
                "/api/v1/accounts",
                &write,
                Some(invalid),
                Some("fails-1"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid");
    }
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("fails-1"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn idempotency_key_header_is_validated() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let too_long = "k".repeat(129);
    for key in ["", "has space", "caf\u{e9}", too_long.as_str()] {
        let mut builder = Request::post("/api/v1/accounts")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", write.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json");
        builder = builder.header(
            "idempotency-key",
            axum::http::HeaderValue::from_bytes(key.as_bytes()).unwrap(),
        );
        let (status, body) = send(
            &world.server,
            builder.body(Body::from(NEW_ACCOUNT)).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?}");
        assert_eq!(body["code"], "invalid");
        assert_eq!(body["detail"], "the `Idempotency-Key` header is not valid");
    }
    let longest = "k".repeat(128);
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some(&longest),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn concurrent_same_key_creates_once() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let post = || {
        router(world.server.app_state()).oneshot(request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("concurrent-1"),
        ))
    };

    let (a, b) = tokio::join!(post(), post());

    let mut results = Vec::new();
    for response in [a.unwrap(), b.unwrap()] {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        results.push((status, body));
    }
    results.sort_by_key(|(status, _)| *status);
    let (first_status, first) = &results[0];
    assert_eq!(*first_status, StatusCode::CREATED);
    let (second_status, second) = &results[1];
    match *second_status {
        StatusCode::CREATED => assert_eq!(second["id"], first["id"]),
        StatusCode::CONFLICT => assert_eq!(second["code"], "idempotency_in_progress"),
        other => panic!("unexpected status {other}"),
    }
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn oversize_body_is_invalid() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let padding = "x".repeat(256 * 1024);
    let body = format!(r#"{{"name": "{padding}", "kind": "savings", "currency": "USD"}}"#);

    let (status, problem) = send(
        &world.server,
        request(Method::POST, "/api/v1/accounts", &write, Some(&body), None),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["code"], "invalid");
    assert_eq!(
        problem["detail"],
        "the request body is not valid JSON for this operation"
    );
    assert_eq!(account_count(&world.server).await, 0);
}
