//! `GET /api/v1/transactions/review-count`: how many transactions wait for a
//! review, in the accounts the token reads.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::AccountId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{World, account, call, read_token, send_raw, transaction, world};

const PATH: &str = "/api/v1/transactions/review-count";

async fn count(world: &World, token: &SecretToken) -> Value {
    let (status, body) = call(&world.server, Method::GET, PATH, token, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// A read token of Alice's holding `transactions` on `account` only.
async fn token_on(world: &World, account: &str) -> SecretToken {
    let (_, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("one account").unwrap(),
            Scope::Read,
            None,
            vec![TokenGrant {
                target: GrantTarget::Account(account.parse::<AccountId>().unwrap()),
                level: AccountLevel::Transactions,
            }],
        )
        .await
        .unwrap();
    secret
}

#[tokio::test]
async fn review_count_counts_only_readable_accounts() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let b = account(&world, "Savings").await;
    transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    transaction(&world, &a, "2026-10-02", -2000, "Market").await;
    transaction(&world, &b, "2026-10-03", -3000, "Bakery").await;

    let read = read_token(&world).await;
    assert_eq!(count(&world, &read).await, json!({"unreviewed": 3}));
    let on_a = token_on(&world, &a).await;
    assert_eq!(count(&world, &on_a).await, json!({"unreviewed": 2}));

    let anonymous = Request::builder()
        .method(Method::GET)
        .uri(PATH)
        .body(Body::empty())
        .unwrap();
    let (status, _) = send_raw(&world.server, anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn review_count_drops_after_review() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    transaction(&world, &a, "2026-10-02", -2000, "Market").await;
    assert_eq!(count(&world, &world.write).await, json!({"unreviewed": 2}));

    let (status, _) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions/batch",
        &world.write,
        Some(json!({"operations": [{"id": first, "patch": {"review": "reviewed"}}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(count(&world, &world.write).await, json!({"unreviewed": 1}));
}
