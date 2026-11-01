//! What a token sees and may do at each grant level, and the uniform access of
//! a token without grants.

use super::{grant, ids, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, TransactionId};
use coffer_domain::policy::{AccountLevel, GrantTarget};
use serde_json::json;

#[tokio::test]
async fn legacy_token_keeps_uniform_access() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Savings").await,
    );
    let (ta, tb) = (
        world.transaction(a, 1, "Groceries").await,
        world.transaction(b, 2, "Rent").await,
    );
    let pa = world.connection(&[("Unbound card", None)]).await;
    for scope in [Scope::Read, Scope::Write] {
        let (_, token) = world.token(scope, Vec::new()).await;
        let (status, accounts) = world.get("/api/v1/accounts", &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&accounts), [s(a), s(b)]);
        let (_, transactions) = world.get("/api/v1/transactions", &token).await;
        assert_eq!(ids(&transactions), [s(tb), s(ta)]);
        let (status, _) = world.get("/api/v1/audit", &token).await;
        assert_eq!(status, StatusCode::OK);
        let (_, provider_accounts) = world.get("/api/v1/provider-accounts", &token).await;
        assert_eq!(ids(&provider_accounts).len(), 1, "unbound ones are listed");
        let (status, _) = world
            .get(&format!("/api/v1/connections/{pa}/sync-runs"), &token)
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    // A write token without grants still creates accounts.
    let (_, write) = world.token(Scope::Write, Vec::new()).await;
    let (status, _) = world
        .call(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(json!({"name": "Brokerage", "kind": "checking", "currency": "USD"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn restricted_token_sees_only_granted_accounts() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    world.balance(b).await;
    // Interleaved, so a hidden row sits between every two visible ones.
    let mut visible = Vec::new();
    for day in 1..=3 {
        visible.push(s(world.transaction(a, day * 2, "Groceries").await));
        world.transaction(b, day * 2 + 1, "Secret").await;
    }
    visible.reverse();
    let hidden_transaction = world.transaction(b, 20, "Secret").await;
    let (_, token) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Transactions)])
        .await;

    let (_, accounts) = world.get("/api/v1/accounts", &token).await;
    assert_eq!(ids(&accounts), [s(a)]);
    let (_, accounts) = world
        .get("/api/v1/accounts?include_closed=true", &token)
        .await;
    assert_eq!(ids(&accounts), [s(a)]);
    let (_, transactions) = world.get("/api/v1/transactions", &token).await;
    assert_eq!(ids(&transactions), visible);

    // Paging one at a time: hidden rows neither appear nor leave a page
    // empty, and the last visible row ends the list.
    let mut paged = Vec::new();
    let mut uri = "/api/v1/transactions?limit=1".to_owned();
    loop {
        let (status, page) = world.get(&uri, &token).await;
        assert_eq!(status, StatusCode::OK);
        let items = ids(&page);
        assert_eq!(items.len(), 1, "no page is empty");
        paged.extend(items);
        match page["next"].as_str() {
            Some(next) => uri = format!("/api/v1/transactions?limit=1&after={next}"),
            None => break,
        }
    }
    assert_eq!(paged, visible);

    // A hidden account answers byte for byte like a missing one.
    let missing = AccountId::new();
    for (hidden, absent) in [
        (
            format!("/api/v1/accounts/{b}"),
            format!("/api/v1/accounts/{missing}"),
        ),
        (
            format!("/api/v1/accounts/{b}/balances"),
            format!("/api/v1/accounts/{missing}/balances"),
        ),
        (
            format!("/api/v1/transactions?account={b}"),
            format!("/api/v1/transactions?account={missing}"),
        ),
        (
            format!("/api/v1/transactions/{hidden_transaction}"),
            format!("/api/v1/transactions/{}", TransactionId::new()),
        ),
    ] {
        let hidden = world.send(Method::GET, &hidden, &token, None, None).await;
        let absent = world.send(Method::GET, &absent, &token, None, None).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND);
        assert!(
            hidden == absent,
            "a hidden resource must answer like a missing one"
        );
    }
}

#[tokio::test]
async fn balances_level_hides_transactions() {
    let world = world().await;
    let a = world.account("Checking").await;
    world.balance(a).await;
    let transaction = world.transaction(a, 1, "Groceries").await;
    let (_, token) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;

    let (status, _) = world.get(&format!("/api/v1/accounts/{a}"), &token).await;
    assert_eq!(status, StatusCode::OK);
    let (status, balances) = world
        .get(&format!("/api/v1/accounts/{a}/balances"), &token)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balances["items"].as_array().unwrap().len(), 1);
    let (status, list) = world.get("/api/v1/transactions", &token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids(&list).is_empty(), "its transactions are not listed");

    for uri in [
        format!("/api/v1/transactions/{transaction}"),
        format!("/api/v1/transactions?account={a}"),
    ] {
        let (status, body) = world.get(&uri, &token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body["detail"],
            "this action needs transactions access to this account"
        );
    }
}

#[tokio::test]
async fn write_needs_write_grant() {
    let world = world().await;
    let (a, c) = (world.account("Checking").await, world.account("Card").await);
    let (ta, tc) = (
        world.transaction(a, 1, "Groceries").await,
        world.transaction(c, 2, "Fuel").await,
    );
    let (_, token) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Transactions),
                on(c, AccountLevel::Write),
            ],
        )
        .await;
    let patch = json!({"review": "reviewed"});

    let (status, body) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/transactions/{ta}"),
            &token,
            Some(patch.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this action needs write access to this account"
    );
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/transactions/{tc}"),
            &token,
            Some(patch),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // A read token's write grant is capped at transactions.
    let (_, reader) = world
        .token(Scope::Read, vec![on(c, AccountLevel::Transactions)])
        .await;
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/accounts/{c}"),
            &reader,
            Some(json!({"name": "Renamed"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn new_accounts_visible_only_through_all_grant() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (_, admin) = world.token(Scope::Write, Vec::new()).await;
    let (_, one) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
        .await;
    let (_, all) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await;

    let created = world.account("Opened later").await;
    let (_, listed) = world.get("/api/v1/accounts", &one).await;
    assert_eq!(ids(&listed), [s(a)]);
    let (status, _) = world
        .get(&format!("/api/v1/accounts/{created}"), &one)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = world.get("/api/v1/accounts", &all).await;
    assert_eq!(ids(&listed), [s(a), s(created)]);

    // A granted account that is closed later stays visible at its level.
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/accounts/{a}"),
            &admin,
            Some(json!({"closed": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, account) = world.get(&format!("/api/v1/accounts/{a}"), &one).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(account["id"], s(a));
    let (_, listed) = world
        .get("/api/v1/accounts?include_closed=true", &one)
        .await;
    assert_eq!(ids(&listed), [s(a)]);
}

#[tokio::test]
async fn restricted_token_cannot_read_audit_or_trigger_sync() {
    let world = world().await;
    let a = world.account("Checking").await;
    let connection = world
        .connection(&[("Everyday", Some(a)), ("Card", None)])
        .await;
    let unbound = world
        .server
        .store
        .list_provider_accounts(Some(connection))
        .await
        .unwrap()
        .into_iter()
        .find(|account| account.account.is_none())
        .unwrap()
        .id;
    let (_, restricted) = world
        .token(Scope::Admin, vec![on(a, AccountLevel::Write)])
        .await;
    let before = world.audit_count().await;

    let refused = [
        (
            Method::GET,
            "/api/v1/audit".to_owned(),
            None,
            "transactions",
        ),
        (
            Method::GET,
            format!("/api/v1/connections/{connection}/sync-runs"),
            None,
            "transactions",
        ),
        (
            Method::POST,
            format!("/api/v1/connections/{connection}/sync"),
            None,
            "write",
        ),
        (
            Method::DELETE,
            format!("/api/v1/connections/{connection}"),
            None,
            "write",
        ),
        (
            Method::POST,
            "/api/v1/accounts".to_owned(),
            Some(json!({"name": "New", "kind": "checking", "currency": "USD"})),
            "write",
        ),
        (
            Method::POST,
            "/api/v1/connections/simplefin".to_owned(),
            Some(json!({"label": "New", "setup_token": "aHR0cHM6Ly9icmlkZ2U"})),
            "write",
        ),
        (
            Method::POST,
            format!("/api/v1/connections/{connection}/simplefin/reconnect"),
            Some(json!({"setup_token": "aHR0cHM6Ly9icmlkZ2U"})),
            "write",
        ),
        (
            Method::POST,
            format!("/api/v1/provider-accounts/{unbound}/bind"),
            Some(json!({"account": a})),
            "write",
        ),
        (
            Method::POST,
            format!("/api/v1/provider-accounts/{unbound}/create-account"),
            Some(json!({})),
            "write",
        ),
    ];
    for (method, uri, body, level) in refused {
        let (status, problem) = world.call(method, &uri, &restricted, body).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "refused without an all grant"
        );
        assert_eq!(
            problem["detail"],
            format!("this action needs {level} access to every account")
        );
    }
    assert_eq!(world.audit_count().await, before, "nothing was written");
}

#[tokio::test]
async fn all_grant_allows_every_account_actions() {
    let world = world().await;
    let a = world.account("Checking").await;
    let connection = world.connection(&[("Everyday", Some(a))]).await;
    let (_, everything) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let (status, _) = world.get("/api/v1/audit", &everything).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{connection}/sync"),
            &everything,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // `all` at transactions reads the audit log but does not sync.
    let (_, reader) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (status, _) = world.get("/api/v1/audit", &reader).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = world
        .call(
            Method::POST,
            "/api/v1/accounts",
            &reader,
            Some(json!({"name": "New", "kind": "checking", "currency": "USD"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
