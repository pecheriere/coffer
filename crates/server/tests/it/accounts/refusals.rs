//! Values and requests the accounts API refuses without echoing them.

use super::{account_count, call, create, request, send, token, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use serde_json::json;

#[tokio::test]
async fn invalid_values_are_refused_without_echo() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    for body in [
        json!({"name": "", "kind": "checking", "currency": "USD"}),
        json!({"name": "SECRET\u{7}", "kind": "checking", "currency": "USD"}),
    ] {
        let (status, problem) = call(
            &world.server,
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], "invalid");
        assert!(
            !problem.to_string().contains("SECRET"),
            "the response must not contain the secret"
        );
    }
    for body in [
        json!({"name": "A", "kind": "SECRETKIND", "currency": "USD"}),
        json!({"name": "A", "kind": "checking", "currency": "SECRET"}),
        json!({"name": "A", "kind": "checking", "currency": "USD", "SECRET": 1}),
    ] {
        let (status, problem) = call(
            &world.server,
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            !problem.to_string().contains("SECRET"),
            "the response must not contain the secret"
        );
    }
    assert_eq!(account_count(&world.server).await, 0);
}

#[tokio::test]
async fn path_rejection_does_not_echo() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    for uri in [
        "/api/v1/accounts/SECRETVALUE",
        "/api/v1/accounts/SECRETVALUE/balances",
    ] {
        let (status, body) = call(&world.server, Method::GET, uri, &write, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["code"], "invalid");
        assert!(
            !body.to_string().contains("SECRET"),
            "the response must not contain the secret"
        );
    }
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, body) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/SECRETDATE"),
        &write,
        Some(json!({"minor_units": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "the `on` parameter is not valid");
    assert!(
        !body.to_string().contains("SECRET"),
        "the response must not contain the secret"
    );
}

#[tokio::test]
async fn unknown_account_is_not_found() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let missing = coffer_domain::ids::AccountId::new();
    for (method, uri, body) in [
        (Method::GET, format!("/api/v1/accounts/{missing}"), None),
        (
            Method::PATCH,
            format!("/api/v1/accounts/{missing}"),
            Some(json!({"name": "X"})),
        ),
        (
            Method::GET,
            format!("/api/v1/accounts/{missing}/balances"),
            None,
        ),
        (
            Method::PUT,
            format!("/api/v1/accounts/{missing}/balances/2026-10-01"),
            Some(json!({"minor_units": 1})),
        ),
    ] {
        let (status, problem) = call(&world.server, method.clone(), &uri, &write, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(problem["code"], "not_found");
    }
}

#[tokio::test]
async fn read_token_cannot_create_account() {
    let world = world().await;
    let (_, read) = token(&world, Scope::Read).await;

    let (status, body) = call(
        &world.server,
        Method::POST,
        "/api/v1/accounts",
        &read,
        Some(json!({"name": "Checking", "kind": "checking", "currency": "USD"})),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(account_count(&world.server).await, 0);

    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &read,
            Some(r#"{"name": "Checking", "kind": "checking", "currency": "USD"}"#),
            Some("read-key"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(account_count(&world.server).await, 0);
}
