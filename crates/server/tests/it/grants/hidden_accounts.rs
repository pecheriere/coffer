//! A hidden account answers like a missing one, wherever it is named.

use super::{World, grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, PlannedItemId};
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

const EVERY_ACCOUNT_AT_TRANSACTIONS: &str =
    "this action needs transactions access to every account";

/// What the forecast's refusal says, in full.
fn forecast_refusal() -> Value {
    json!({
        "type": "urn:coffer:problem:forbidden",
        "title": "Forbidden",
        "status": 403,
        "code": "forbidden",
        "detail": EVERY_ACCOUNT_AT_TRANSACTIONS,
    })
}

/// A what-if imagining one roof, hinted at `hint`.
fn roof(hint: Option<AccountId>) -> Value {
    json!({"items": [{
        "name": "Roof",
        "amount": {"minor_units": 100, "currency": "USD"},
        "direction": "outflow",
        "account_hint": hint,
        "schedule": {"type": "one_off", "on": "2026-12-20"},
    }]})
}

impl World {
    async fn what_if(&self, token: &SecretToken, body: &Value) -> (StatusCode, String) {
        self.send(
            Method::POST,
            "/api/v1/forecast/what-if",
            token,
            Some(body),
            None,
        )
        .await
    }

    /// A household a forecast would have something to say about: a hidden
    /// account with a large and stale balance, a provider account bound to
    /// nothing, and a safety floor.
    async fn hidden_fortune(&self) -> AccountId {
        let hidden = self.account("Fortune").await;
        self.server
            .ledger
            .set_manual_balance(
                &self.alice,
                hidden,
                jiff::civil::date(2020, 1, 1),
                987_654_321,
            )
            .await
            .unwrap();
        self.connection(&[("Unbound fortune", None)]).await;
        hidden
    }
}

#[tokio::test]
async fn forecast_never_answers_a_restricted_token() {
    let world = world().await;
    let visible = world.account("Checking").await;
    let hidden = world.hidden_fortune().await;
    for grants in [
        vec![on(visible, AccountLevel::Write)],
        vec![grant(GrantTarget::All, AccountLevel::Balances)],
        vec![
            grant(GrantTarget::All, AccountLevel::Balances),
            on(visible, AccountLevel::Write),
        ],
    ] {
        let (_, token) = world.token(Scope::Write, grants).await;
        let (status, text) = world
            .send(Method::GET, "/api/v1/forecast", &token, None, None)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body, forecast_refusal());
        for held_back in ["Fortune", "Unbound fortune", "987654321", "2020-01-01"] {
            assert!(!text.contains(held_back), "the refusal says nothing more");
        }
        assert!(!text.contains(&s(hidden)), "the refusal names nothing");
    }
}

#[tokio::test]
async fn what_if_with_hidden_hint_is_refused_like_any_restriction() {
    let world = world().await;
    let visible = world.account("Checking").await;
    let hidden = world.hidden_fortune().await;
    let (_, token) = world
        .token(Scope::Read, vec![on(visible, AccountLevel::Transactions)])
        .await;

    let none = world.what_if(&token, &roof(None)).await;
    let naming_visible = world.what_if(&token, &roof(Some(visible))).await;
    let naming_hidden = world.what_if(&token, &roof(Some(hidden))).await;

    assert_eq!(none.0, StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_str(&none.1).unwrap();
    assert_eq!(body, forecast_refusal());
    assert!(naming_visible == none, "the authorization comes first");
    assert!(naming_hidden == none, "the authorization comes first");
}

#[tokio::test]
async fn what_if_cannot_probe_account_existence() {
    let world = world().await;
    let visible = world.account("Checking").await;
    let hidden = world.hidden_fortune().await;
    let (_, token) = world
        .token(Scope::Write, vec![on(visible, AccountLevel::Write)])
        .await;
    let audited = world.audit_count().await;

    let naming_hidden = world.what_if(&token, &roof(Some(hidden))).await;
    let naming_missing = world.what_if(&token, &roof(Some(AccountId::new()))).await;

    assert_eq!(naming_hidden.0, StatusCode::FORBIDDEN);
    assert!(
        naming_hidden == naming_missing,
        "a hidden hint answers like a missing one"
    );
    assert_eq!(world.audit_count().await, audited, "nothing was written");

    // The token that may ask is told a missing account is no hint, so it is the
    // restricted one's refusal that keeps the difference from showing.
    let (_, wide) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (status, _) = world.what_if(&wide, &roof(Some(AccountId::new()))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Every way a hidden item could fail to be linked answers as a missing goal,
/// on a transaction already linked to it too.
#[tokio::test]
async fn goal_link_cannot_probe_hidden_items() {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let category = world.expense_category().await;
    let linked_goal = world
        .planned_item("Linked", Some(hidden), category, 5000)
        .await;
    let free_goal = world
        .planned_item("Free", Some(hidden), category, 5000)
        .await;
    let not_a_goal = {
        let input = serde_json::from_value(json!({
            "name": "Not a goal",
            "category": category,
            "amount": {"minor_units": 5000, "currency": "USD"},
            "direction": "outflow",
            "account_hint": hidden,
            "schedule": {"type": "one_off", "on": "2027-03-15"},
        }))
        .unwrap();
        world
            .server
            .ledger
            .create_planned_item(&world.alice, input, None)
            .await
            .unwrap()
            .id
    };
    let other_currency = {
        let input = serde_json::from_value(json!({
            "name": "In euros",
            "category": category,
            "amount": {"minor_units": 5000, "currency": "EUR"},
            "direction": "outflow",
            "account_hint": hidden,
            "schedule": {"type": "goal", "start_on": "2026-11-01", "due_on": "2027-11-01"},
        }))
        .unwrap();
        world
            .server
            .ledger
            .create_planned_item(&world.alice, input, None)
            .await
            .unwrap()
            .id
    };
    let transaction = world.transaction(visible, 1, "Flights").await;
    world
        .link_split(transaction, category, -1000, linked_goal)
        .await;
    let (_, token) = world
        .token(Scope::Write, vec![on(visible, AccountLevel::Write)])
        .await;
    let uri = format!("/api/v1/transactions/{transaction}");
    let naming = |goal: PlannedItemId| json!({"splits": [{"category": category, "minor_units": -1000, "goal": goal}]});
    let missing = world
        .send(
            Method::PATCH,
            &uri,
            &token,
            Some(&naming(PlannedItemId::new())),
            None,
        )
        .await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&missing.1).unwrap();
    assert_eq!(
        body,
        json!({
            "type": "urn:coffer:problem:invalid",
            "title": "Invalid value",
            "status": 422,
            "code": "invalid",
            "detail": "a split names a goal that does not exist",
        })
    );

    for goal in [linked_goal, free_goal, not_a_goal, other_currency] {
        let answer = world
            .send(Method::PATCH, &uri, &token, Some(&naming(goal)), None)
            .await;
        assert!(
            answer == missing,
            "a hidden item answers like a missing goal"
        );
    }
}
