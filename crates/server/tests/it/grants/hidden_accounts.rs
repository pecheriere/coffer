//! A hidden account answers like a missing one, wherever it is named.

use super::{grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::{AccountLevel, GrantTarget};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

#[tokio::test]
async fn binding_to_a_hidden_account_answers_like_a_missing_one() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let connection = world.connection(&[("Card", None)]).await;
    let unbound = world
        .server
        .store
        .list_provider_accounts(Some(connection))
        .await
        .unwrap()[0]
        .id;
    let (_, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let uri = format!("/api/v1/provider-accounts/{unbound}/bind");
    let hidden = world
        .send(
            Method::POST,
            &uri,
            &token,
            Some(&json!({"account": b})),
            None,
        )
        .await;
    let absent = world
        .send(
            Method::POST,
            &uri,
            &token,
            Some(&json!({"account": AccountId::new()})),
            None,
        )
        .await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert!(
        hidden == absent,
        "a hidden account must answer like a missing one"
    );
}

#[tokio::test]
async fn provider_accounts_follow_their_accounts() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let connection = world
        .connection(&[
            ("Everyday", Some(a)),
            ("Secret savings", Some(b)),
            ("Unbound card", None),
        ])
        .await;
    let names = |body: &Value| -> Vec<String> {
        body.as_array()
            .map_or_else(|| body["items"].as_array().unwrap().clone(), Clone::clone)
            .iter()
            .map(|account| account["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let connection_names = |body: &Value| -> Vec<String> {
        let list = body
            .as_array()
            .map_or_else(|| body["items"].as_array().unwrap().clone(), Clone::clone);
        let entry = list
            .iter()
            .find(|entry| entry["id"] == s(connection))
            .unwrap();
        names(&entry["accounts"])
    };
    for (grants, expected) in [
        (vec![on(a, AccountLevel::Balances)], vec!["Everyday"]),
        (
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
            vec!["Everyday", "Secret savings"],
        ),
        (
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
            vec!["Everyday", "Secret savings", "Unbound card"],
        ),
    ] {
        let (_, token) = world.token(Scope::Read, grants).await;
        let (status, listed) = world.get("/api/v1/provider-accounts", &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(names(&listed), expected);
        let (status, connections) = world.get("/api/v1/connections", &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(connection_names(&connections), expected);
    }
}

#[tokio::test]
async fn batch_names_a_hidden_transaction_by_index_only() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let (ta, tb) = (
        world.transaction(a, 1, "Groceries").await,
        world.transaction(b, 2, "Secret").await,
    );
    let (_, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let before = world.audit_count().await;
    let (status, text) = world
        .send(
            Method::POST,
            "/api/v1/transactions/batch",
            &token,
            Some(&json!({"operations": [
                {"id": ta, "patch": {"review": "reviewed"}},
                {"id": tb, "patch": {"review": "reviewed"}},
            ]})),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["detail"], "operations[1]: not found");
    assert!(!text.contains(&s(tb)), "the hidden id is not echoed");
    assert_eq!(
        world.audit_count().await,
        before,
        "the batch was not applied"
    );
}

#[tokio::test]
async fn merge_across_hidden_or_lower_accounts_is_refused() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Other").await,
    );
    let (ta, tb) = (
        world.transaction(a, 1, "Groceries").await,
        world.transaction(b, 1, "Groceries").await,
    );
    let uri = format!("/api/v1/transactions/{ta}/merge");
    let body = json!({"duplicate": tb});
    let (_, hidden) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let (_, lower) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Write),
                on(b, AccountLevel::Transactions),
            ],
        )
        .await;
    let before = world.audit_count().await;

    let (status, text) = world
        .send(Method::POST, &uri, &hidden, Some(&body), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !text.contains(&s(ta)) && !text.contains(&s(tb)),
        "neither id is in the body"
    );
    let (status, _) = world
        .send(Method::POST, &uri, &lower, Some(&body), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(world.audit_count().await, before, "nothing audited");
}

#[tokio::test]
async fn hidden_account_create_is_refused_before_the_key_is_reserved() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let (_, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let body = |account: AccountId| {
        json!({"account": account, "posted_on": "2026-10-05", "minor_units": -500,
               "description": "Coffee"})
    };
    let missing = AccountId::new();
    let absent = world
        .send(
            Method::POST,
            "/api/v1/transactions",
            &token,
            Some(&body(missing)),
            Some("key-missing"),
        )
        .await;
    for _ in 0..2 {
        // The same key twice: never `in progress`, never a replay.
        let hidden = world
            .send(
                Method::POST,
                "/api/v1/transactions",
                &token,
                Some(&body(b)),
                Some("key-1"),
            )
            .await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND);
        assert!(hidden == absent, "answered like a missing account");
    }
    // The key is free: the same key on a visible account creates.
    let (status, _) = world
        .send(
            Method::POST,
            "/api/v1/transactions",
            &token,
            Some(&body(a)),
            Some("key-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn idempotency_keys_are_never_replayed_to_another_token() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Other").await,
    );
    let (_, wide) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let (_, narrow) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let (_, blind) = world
        .token(Scope::Write, vec![on(b, AccountLevel::Write)])
        .await;
    let request = json!({"account": a, "posted_on": "2026-10-05", "minor_units": -500,
                         "description": "Coffee"});
    let post = async |token: &SecretToken| {
        world
            .send(
                Method::POST,
                "/api/v1/transactions",
                token,
                Some(&request),
                Some("shared-key"),
            )
            .await
    };

    let first = post(&wide).await;
    assert_eq!(first.0, StatusCode::CREATED);
    assert!(post(&wide).await == first, "the same token gets a replay");

    // A token that cannot see the account gets its own refusal.
    let refused = post(&blind).await;
    assert_eq!(refused.0, StatusCode::NOT_FOUND);
    // A narrower token that may make the request gets its own outcome.
    let own = post(&narrow).await;
    assert_eq!(own.0, StatusCode::CREATED);
    let first: Value = serde_json::from_str(&first.1).unwrap();
    let own: Value = serde_json::from_str(&own.1).unwrap();
    assert_ne!(own["id"], first["id"], "not the other token's response");
}
