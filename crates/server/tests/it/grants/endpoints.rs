//! Grants enforced on the endpoints that are not plain account reads.

use super::{grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::ledger::categories::CategoryGroupInput;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::json;
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

/// The server makes candidate pairs within one account only, and merges
/// only within one account, so these cross-account pairs are seeded. Should
/// one ever exist, a token that cannot read the other account's transactions
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
