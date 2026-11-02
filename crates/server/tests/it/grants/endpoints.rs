//! Grants enforced on the endpoints that are not plain account reads.

use super::{grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::ledger::categories::CategoryGroupInput;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn last_error_needs_every_account_at_transactions() {
    let world = world().await;
    let a = world.account("Checking").await;
    let connection = world.connection(&[("Everyday", Some(a))]).await;
    world
        .fail_connection(connection, "the bank refused account Secret")
        .await;
    let last_error = async |grants: Vec<TokenGrant>| {
        let (_, token) = world.token(Scope::Write, grants).await;
        let (status, body) = world.get("/api/v1/connections", &token).await;
        assert_eq!(status, StatusCode::OK);
        let listed = &body["items"][0];
        assert_eq!(listed["status"], "error", "the status always shows");
        listed["last_error"].as_str().map(str::to_owned)
    };

    let error = Some("the bank refused account Secret".to_owned());
    assert_eq!(last_error(Vec::new()).await, error, "a uniform token");
    assert_eq!(
        last_error(vec![grant(GrantTarget::All, AccountLevel::Transactions)]).await,
        error
    );
    assert_eq!(
        last_error(vec![on(a, AccountLevel::Write)]).await,
        None,
        "write on one account is not every account"
    );
    assert_eq!(
        last_error(vec![grant(GrantTarget::All, AccountLevel::Balances)]).await,
        None
    );
}

/// A create needing `all` at `write` is refused before its key is reserved:
/// once the token may, the same key creates.
#[tokio::test]
async fn connection_and_category_creates_refuse_before_reserving_the_key() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let requests = [
        ("/api/v1/category-groups", json!({"name": "Home"})),
        (
            "/api/v1/connections/simplefin",
            json!({"label": "New", "setup_token": "a-setup-token"}),
        ),
    ];
    for (uri, body) in &requests {
        let (status, _) = world
            .send(Method::POST, uri, &token, Some(body), Some(uri))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await
        .unwrap();
    for (uri, body) in &requests {
        let (status, _) = world
            .send(Method::POST, uri, &token, Some(body), Some(uri))
            .await;
        assert_eq!(status, StatusCode::CREATED, "the key was free");
    }
}

#[tokio::test]
async fn category_writes_need_every_account_at_write() {
    let world = world().await;
    let a = world.account("Checking").await;
    let transaction = world.transaction(a, 1, "Coffee").await;
    let (_, uniform) = world.token(Scope::Write, Vec::new()).await;
    let (_, everything) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let (_, one_account) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let (_, read_all) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;

    let (status, group) = world
        .call(
            Method::POST,
            "/api/v1/category-groups",
            &uniform,
            Some(json!({"name": "Home"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "a uniform token creates");
    let group = group["id"].as_str().unwrap().to_owned();
    let (status, category) = world
        .call(
            Method::POST,
            "/api/v1/categories",
            &everything,
            Some(json!({"group": group, "name": "Rent", "kind": "expense"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "an all grant creates");
    let category = category["id"].as_str().unwrap().to_owned();

    let before = world.audit_count().await;
    let writes = [
        (
            Method::POST,
            "/api/v1/category-groups".to_owned(),
            json!({"name": "Other"}),
        ),
        (
            Method::PATCH,
            format!("/api/v1/category-groups/{group}"),
            json!({"archived": true}),
        ),
        (
            Method::POST,
            "/api/v1/categories".to_owned(),
            json!({"group": group, "name": "Other", "kind": "expense"}),
        ),
        (
            Method::PATCH,
            format!("/api/v1/categories/{category}"),
            json!({"archived": true}),
        ),
    ];
    for token in [&one_account, &read_all] {
        for (method, uri, body) in &writes {
            let (status, problem) = world
                .call(method.clone(), uri, token, Some(body.clone()))
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(
                problem["detail"],
                "this action needs write access to every account"
            );
        }
    }
    assert_eq!(world.audit_count().await, before, "nothing was written");

    // Using an existing category on a granted account is that account's
    // business.
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/transactions/{transaction}"),
            &one_account,
            Some(json!({"splits": [{"category": category, "minor_units": -1000}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "a split on a granted account");

    // A session has uniform access.
    world
        .server
        .ledger
        .create_category_group(
            &world.alice,
            CategoryGroupInput {
                name: "From a session".to_owned(),
                position: None,
            },
            None,
        )
        .await
        .unwrap();
}

/// A token that lost an account gets a 404 for its own earlier request, not
/// the response it stored while it could see the account.
#[tokio::test]
async fn own_replay_stops_once_the_account_is_hidden() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Other").await,
    );
    let (id, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let request = json!({"account": a, "posted_on": "2026-10-05", "minor_units": -500,
                         "description": "Coffee"});
    let post = async || {
        world
            .send(
                Method::POST,
                "/api/v1/transactions",
                &token,
                Some(&request),
                Some("the-key"),
            )
            .await
    };
    let first = post().await;
    assert_eq!(first.0, StatusCode::CREATED);
    assert!(
        post().await == first,
        "a replay while the account is visible"
    );

    let mut tx = world.server.store.begin().await.unwrap();
    tx.replace_token_grants(id, &[on(b, AccountLevel::Write)])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let retry = post().await;
    assert_eq!(retry.0, StatusCode::NOT_FOUND, "not the stored response");
    assert!(!retry.1.contains("Coffee"), "nothing of the stored body");
}

/// Claiming a setup token spends it, so once the provider has answered
/// nothing may turn the reconnect into a failure: access is resolved first.
#[tokio::test]
async fn reconnect_resolves_access_before_spending_the_setup_token() {
    let world = world().await;
    let connection = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Family bank",
            world.alice.member,
            "seeded-access",
        )
        .await;
    let (id, secret) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let agent = world
        .server
        .agent_tokens
        .resolve(secret.expose_secret())
        .await
        .unwrap();
    let gate = world.fake.hold_next_connect();
    let sync = Arc::clone(&world.server.sync);
    let reconnecting = tokio::spawn(async move {
        sync.reconnect(
            &agent,
            connection,
            ConnectInput::SimpleFinSetupToken(SecretString::from("new-token")),
        )
        .await
    });
    gate.entered().await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "INSERT INTO agent_token_grants (token_id, target, level) \
             VALUES (?, 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', 'write')",
            &s(id),
        )
        .await
        .unwrap();
    gate.release();
    assert!(
        reconnecting.await.unwrap().is_ok(),
        "the reconnect that spent the token succeeds"
    );
}

/// The store refuses candidate pairs across accounts, so these are seeded
/// past that check as defence in depth. Should one ever exist, a token that cannot read the other account's transactions
/// is not told its id, by any answer that shows the candidates.
#[tokio::test]
async fn duplicate_candidates_in_hidden_accounts_are_not_named() {
    let world = world().await;
    let (a, b, c) = (
        world.account("Checking").await,
        world.account("Hidden").await,
        world.account("Balances only").await,
    );
    let flagged = world.transaction(a, 1, "Coffee").await;
    let same_account = world.transaction(a, 2, "Coffee again").await;
    let hidden = world.transaction(b, 1, "Coffee elsewhere").await;
    let low = world.transaction(c, 1, "Coffee at balances").await;
    for candidate in [same_account, hidden, low] {
        world.flag_duplicate(flagged, candidate).await;
    }

    let (_, restricted) = world
        .token(
            Scope::Write,
            vec![on(a, AccountLevel::Write), on(c, AccountLevel::Balances)],
        )
        .await;
    let (_, list) = world.get("/api/v1/transactions", &restricted).await;
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == s(flagged))
        .unwrap();
    assert_eq!(item["duplicate_candidates"], json!([s(same_account)]));
    let uri = format!("/api/v1/transactions/{flagged}");
    let (_, got) = world.get(&uri, &restricted).await;
    assert_eq!(got["duplicate_candidates"], json!([s(same_account)]));
    assert_eq!(got["flags"], json!(["possible_duplicate"]));
    let (status, patched) = world
        .call(
            Method::PATCH,
            &uri,
            &restricted,
            Some(json!({"notes": "looked"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["duplicate_candidates"], json!([s(same_account)]));
    let (status, batch) = world
        .call(
            Method::POST,
            "/api/v1/transactions/batch",
            &restricted,
            Some(json!({"operations": [{"id": flagged, "patch": {"notes": "again"}}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        batch["items"][0]["duplicate_candidates"],
        json!([s(same_account)])
    );

    // One that reads every account is told all of them.
    let (_, wide) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (_, got) = world.get(&uri, &wide).await;
    assert_eq!(got["duplicate_candidates"].as_array().unwrap().len(), 3);

    // A merge answers with the target's candidates, filtered the same way.
    // Three connections, because a merge forgets a candidate that has a
    // posted record from the same connection as the merged transaction.
    let target_feed = world.connection(&[("Target", Some(a))]).await;
    let duplicate_feed = world.connection(&[("Duplicate", Some(a))]).await;
    let candidates_feed = world
        .connection(&[("Hidden", Some(b)), ("Low", Some(c))])
        .await;
    let (target, _) = world.imported(target_feed, a, "Groceries").await;
    let (duplicate, _) = world.imported(duplicate_feed, a, "Groceries").await;
    let (hidden, _) = world.imported(candidates_feed, b, "Groceries").await;
    let (low, _) = world.imported(candidates_feed, c, "Groceries").await;
    for candidate in [duplicate, hidden, low] {
        world.flag_duplicate(target, candidate).await;
    }
    let merge = format!("/api/v1/transactions/{target}/merge");
    let (status, merged) = world
        .call(
            Method::POST,
            &merge,
            &restricted,
            Some(json!({"duplicate": duplicate})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(merged["duplicate_candidates"], json!([]));
    let (_, got) = world
        .get(&format!("/api/v1/transactions/{target}"), &wide)
        .await;
    assert_eq!(
        got["duplicate_candidates"].as_array().unwrap().len(),
        2,
        "the candidates survived the merge: the filter hid them"
    );
}

/// A create body for a monthly bill, hinted at `hint`.
fn bill(category: CategoryId, hint: Option<AccountId>) -> Value {
    json!({
        "name": "Water bill",
        "category": category,
        "amount": {"minor_units": 4200, "currency": "USD"},
        "direction": "outflow",
        "account_hint": hint,
        "schedule": {"type": "recurring", "every": 1, "unit": "months",
                     "anchor_on": "2027-01-01"},
    })
}

const PLAN_ITEMS: &str = "/api/v1/plan-items";

#[tokio::test]
async fn plan_writes_need_every_account_at_write_before_reserving_the_key() {
    let world = world().await;
    let a = world.account("Checking").await;
    let category = world.expense_category().await;
    let (id, token) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let body = bill(category, Some(a));
    let before = world.audit_count().await;
    for _ in 0..2 {
        // The same key twice: never `in progress`, never a replay.
        let (status, text) = world
            .send(
                Method::POST,
                PLAN_ITEMS,
                &token,
                Some(&body),
                Some("plan-key"),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let refusal: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            refusal,
            json!({
                "type": "urn:coffer:problem:forbidden",
                "title": "Forbidden",
                "status": 403,
                "code": "forbidden",
                "detail": "this action needs write access to every account",
            })
        );
    }
    assert_eq!(world.audit_count().await, before, "nothing was written");

    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await
        .unwrap();
    let (status, _) = world
        .send(
            Method::POST,
            PLAN_ITEMS,
            &token,
            Some(&body),
            Some("plan-key"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "the key was free");
}

/// `all` at `balances` shows every account and its balances, and nothing a
/// spending figure could be worked out from.
#[tokio::test]
async fn balances_level_grant_on_all_cannot_derive_spending() {
    let world = world().await;
    let a = world.account("Checking").await;
    world.balance(a).await;
    let category = world.expense_category().await;
    let goal = world
        .planned_item("Spending plan", Some(a), category, 5000)
        .await;
    let transaction = world.transaction(a, 1, "A purchase").await;
    world.link_split(transaction, category, -1000, goal).await;
    let (_, token) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await;

    let (status, text) = world
        .send(Method::GET, "/api/v1/transactions", &token, None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed, json!({"items": [], "next": null}));
    let (status, text) = world
        .send(Method::GET, PLAN_ITEMS, &token, None, None)
        .await;
    assert_eq!((status, text.as_str()), (StatusCode::OK, r#"{"items":[]}"#));
    let refusal = json!({
        "type": "urn:coffer:problem:forbidden",
        "title": "Forbidden",
        "status": 403,
        "code": "forbidden",
        "detail": "this action needs transactions access to every account",
    });
    for uri in [
        "/api/v1/forecast",
        "/api/v1/reports/plan-vs-actual?from=2026-10",
        "/api/v1/audit",
    ] {
        let (status, body) = world.get(uri, &token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body, refusal);
    }
    let (status, _) = world
        .get(&format!("/api/v1/transactions/{transaction}"), &token)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a level too low, named");
}

#[tokio::test]
async fn grant_change_applies_to_plan_at_next_request() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Other").await,
    );
    let category = world.expense_category().await;
    let item = world
        .planned_item("On the first", Some(a), category, 5000)
        .await;
    let (id, token) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;
    let uri = format!("{PLAN_ITEMS}/{item}");
    let (status, _) = world.get(&uri, &token).await;
    assert_eq!(status, StatusCode::OK, "the hint is visible");

    world
        .server
        .agent_tokens
        .replace_grants(&world.alice, id, vec![on(b, AccountLevel::Transactions)])
        .await
        .unwrap();

    let answer = world.send(Method::GET, &uri, &token, None, None).await;
    let missing = world
        .send(
            Method::GET,
            &format!("{PLAN_ITEMS}/{}", coffer_domain::ids::PlannedItemId::new()),
            &token,
            None,
            None,
        )
        .await;
    assert_eq!(answer.0, StatusCode::NOT_FOUND);
    assert!(answer == missing, "answered like a missing item");
    let (_, listed) = world.get(PLAN_ITEMS, &token).await;
    assert_eq!(listed, json!({"items": []}));
}

/// An account made after a token is hidden from it, with the items hinted at
/// it, unless it holds `all`.
#[tokio::test]
async fn new_account_hint_needs_an_all_grant() {
    let world = world().await;
    let a = world.account("Checking").await;
    let category = world.expense_category().await;
    let (_, named) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;
    let (_, everything) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;

    let newer = world.account("Newer").await;
    let item = world
        .planned_item("On the newer", Some(newer), category, 5000)
        .await;

    let uri = format!("{PLAN_ITEMS}/{item}");
    let (status, _) = world.get(&uri, &named).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, seen) = world.get(&uri, &everything).await;
    assert_eq!(status, StatusCode::OK);
    let hint = seen["account_hint"].clone();
    assert_eq!(hint, s(newer));
}

#[tokio::test]
async fn audit_of_plan_changes_stays_behind_the_all_grant() {
    let world = world().await;
    let a = world.account("Checking").await;
    let category = world.expense_category().await;
    let (_, writer) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let (status, created) = world
        .call(
            Method::POST,
            PLAN_ITEMS,
            &writer,
            Some(bill(category, Some(a))),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let item = created["id"].as_str().unwrap().to_owned();
    let (status, _) = world
        .call(
            Method::DELETE,
            &format!("{PLAN_ITEMS}/{item}"),
            &writer,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let filtered = format!("/api/v1/audit?entity_type=planned_item&entity_id={item}");
    for grants in [
        vec![on(a, AccountLevel::Write)],
        vec![grant(GrantTarget::All, AccountLevel::Balances)],
    ] {
        let (_, token) = world.token(Scope::Write, grants).await;
        for uri in ["/api/v1/audit", filtered.as_str()] {
            let (status, text) = world.send(Method::GET, uri, &token, None, None).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                body["detail"],
                "this action needs transactions access to every account"
            );
            assert!(!text.contains("Water bill"), "nothing of what it records");
        }
    }

    let (_, wide) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (status, entries) = world.get(&filtered, &wide).await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<&str> = entries["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["planned_item.delete", "planned_item.create"]);
}
