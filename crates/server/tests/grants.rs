//! Per-account grants of agent tokens, enforced through the real router
//! (spec §8.2): a token with grants sees only the accounts they name, at
//! their levels; a hidden account answers exactly like a missing one; the
//! actions of the policy's rule table need an `all` grant; and no response
//! reveals a hidden account.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::BTreeMap;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{
    AccountLevel, AccountScope, DenyReason, GrantError, GrantTarget, TokenGrant, authorize_account,
    grant_ceiling,
};
use coffer_domain::time::Clock as _;
use coffer_server::auth::AuthError;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::transactions::NewManualTransactionInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditFilter, NewConnection, PageRequest, ProviderAccountUpsert};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use proptest::strategy::Strategy as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const LEVELS: [AccountLevel; 3] = [
    AccountLevel::Balances,
    AccountLevel::Transactions,
    AccountLevel::Write,
];

struct World {
    server: TestServer,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let server = TestServer::with_fake_provider().await.0;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    World {
        server,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

fn grant(target: GrantTarget, level: AccountLevel) -> TokenGrant {
    TokenGrant { target, level }
}

fn on(account: AccountId, level: AccountLevel) -> TokenGrant {
    grant(GrantTarget::Account(account), level)
}

impl World {
    /// A token of alice's with `scope` and `grants`.
    async fn token(&self, scope: Scope, grants: Vec<TokenGrant>) -> (AgentTokenId, SecretToken) {
        let (record, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap();
        (record.id, secret)
    }

    async fn account(&self, name: &str) -> AccountId {
        self.server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Checking,
                    currency: Currency::parse("USD").unwrap(),
                    liquidity: None,
                    include_in_net_worth: true,
                    include_in_forecast: true,
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    async fn transaction(&self, account: AccountId, day: i8, description: &str) -> TransactionId {
        self.server
            .ledger
            .create_manual_transaction(
                &self.alice,
                NewManualTransactionInput {
                    account,
                    posted_on: date(2026, 10, day),
                    minor_units: -1000,
                    description: description.into(),
                    payee: None,
                    memo: None,
                    notes: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    async fn balance(&self, account: AccountId) {
        self.server
            .ledger
            .set_manual_balance(&self.alice, account, date(2026, 10, 1), 50_000)
            .await
            .unwrap();
    }

    /// A connection reporting one provider account per entry of
    /// `accounts`: its name, and the account it is bound to.
    async fn connection(&self, accounts: &[(&str, Option<AccountId>)]) -> ConnectionId {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        let connection = tx
            .insert_connection(NewConnection {
                provider: ProviderKind::SimpleFin.as_str(),
                label: ConnectionLabel::parse("Family bank").unwrap(),
                owner: self.alice.member,
                credentials: vec![1, 2, 3],
                created_at: now,
            })
            .await
            .unwrap();
        for (n, (name, bound)) in accounts.iter().enumerate() {
            let record = tx
                .upsert_provider_account(ProviderAccountUpsert {
                    connection: connection.id,
                    provider_account_id: format!("acct-{n}"),
                    name: (*name).to_owned(),
                    institution: None,
                    currency_raw: "USD".into(),
                    kind_hint: Some(AccountKind::Checking),
                    balance: None,
                    seen_at: now,
                })
                .await
                .unwrap();
            if let Some(account) = bound {
                tx.bind_provider_account(record.id, *account).await.unwrap();
            }
        }
        tx.commit().await.unwrap();
        connection.id
    }

    async fn audit_count(&self) -> usize {
        self.server
            .store
            .list_audit(
                &AuditFilter::default(),
                &PageRequest {
                    limit: 200,
                    ..PageRequest::default()
                },
            )
            .await
            .unwrap()
            .items
            .len()
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, text) = self.send(method, uri, token, body.as_ref(), None).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, uri: &str, token: &SecretToken) -> (StatusCode, Value) {
        self.call(Method::GET, uri, token, None).await
    }
}

/// The `id`s of a list response's `items`, or of a bare array.
fn ids(body: &Value) -> Vec<String> {
    body.get("items")
        .unwrap_or(body)
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

fn s(id: impl ToString + Copy) -> String {
    id.to_string()
}

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

#[tokio::test]
async fn token_grants_are_validated() {
    let world = world().await;
    let a = world.account("Checking").await;
    let create = async |scope: Scope, grants: Vec<TokenGrant>| {
        world
            .server
            .agent_tokens
            .create(
                &world.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
    };
    assert!(matches!(
        create(Scope::Read, vec![on(a, AccountLevel::Write)]).await,
        Err(AuthError::InvalidGrants(GrantError::AboveScope))
    ));
    assert!(matches!(
        create(
            Scope::Write,
            vec![on(a, AccountLevel::Write), on(a, AccountLevel::Balances)]
        )
        .await,
        Err(AuthError::InvalidGrants(GrantError::DuplicateTarget))
    ));
    let unknown = create(
        Scope::Write,
        vec![on(AccountId::new(), AccountLevel::Write)],
    )
    .await;
    assert!(matches!(unknown, Err(AuthError::UnknownGrantedAccount)));
    assert_eq!(
        unknown.err().unwrap().to_string(),
        "a granted account does not exist"
    );
    let (id, _) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Write),
                grant(GrantTarget::All, AccountLevel::Balances),
            ],
        )
        .await;
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [
            grant(GrantTarget::All, AccountLevel::Balances),
            on(a, AccountLevel::Write),
        ]
    );
    let events = world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_id: Some(s(id)),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items;
    let (_, recorded) = events[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "grants")
        .unwrap();
    assert_eq!(
        recorded.after,
        AuditValue::from(format!("all=balances,{a}=write").as_str()),
        "the audit entry holds the canonical grants"
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn level() -> impl proptest::strategy::Strategy<Value = AccountLevel> {
    proptest::sample::select(LEVELS.to_vec())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]

    /// Grants as stored, unvalidated (above the scope included), resolved
    /// for every role and token scope: `scope()` agrees with
    /// `authorize_account`, no level exceeds the scope's ceiling, and an
    /// account no grant names is hidden. A session ignores stored grants.
    #[test]
    fn resolver_never_exceeds_the_scope_ceiling(
        role in proptest::sample::select(vec![Role::Member, Role::Admin]),
        scope in proptest::sample::select(vec![Scope::Read, Scope::Write, Scope::Admin]),
        all in proptest::option::of(level()),
        named in proptest::collection::vec(proptest::option::of(level()), 3),
    ) {
        runtime().block_on(async {
            let world = world().await;
            let mut accounts = Vec::new();
            for n in 0..4 {
                accounts.push(world.account(&format!("Account {n}")).await);
            }
            let (token, _) = world.token(Scope::Admin, Vec::new()).await;
            let mut grants = BTreeMap::new();
            if let Some(level) = all {
                grants.insert(GrantTarget::All, level);
            }
            // The fourth account is never named.
            for (account, level) in accounts.iter().zip(&named) {
                if let Some(level) = level {
                    grants.insert(GrantTarget::Account(*account), *level);
                }
            }
            let grants: Vec<TokenGrant> = grants
                .into_iter()
                .map(|(target, level)| TokenGrant { target, level })
                .collect();
            let mut tx = world.server.store.begin().await.unwrap();
            tx.replace_token_grants(token, &grants).await.unwrap();
            tx.commit().await.unwrap();

            let actor = Actor { member: world.alice.member, role, via: Via::Agent { token, scope } };
            let access = world.server.access.resolve(&actor).await.unwrap();
            let ceiling = grant_ceiling(actor.effective_scope());
            for (n, account) in accounts.iter().enumerate() {
                let named_level = named.get(n).copied().flatten();
                let best = all.max(named_level);
                let level = access.level_for(*account);
                if grants.is_empty() {
                    proptest::prop_assert_eq!(level, Some(ceiling));
                } else {
                    proptest::prop_assert_eq!(level, best.map(|best| best.min(ceiling)));
                }
                if level.is_none() {
                    let denied = authorize_account(&actor, &access, *account, AccountLevel::Balances);
                    proptest::prop_assert_eq!(denied.map_err(|d| d.reason), Err(DenyReason::AccountHidden));
                }
                for needed in LEVELS {
                    let allowed = authorize_account(&actor, &access, *account, needed).is_ok();
                    let listed = match access.scope(needed) {
                        AccountScope::All => true,
                        AccountScope::Only(ids) => ids.contains(account),
                    };
                    proptest::prop_assert_eq!(listed, allowed);
                    if needed > ceiling {
                        proptest::prop_assert!(!allowed);
                        proptest::prop_assert!(!access.sees_every_account(needed));
                    }
                }
            }
            let session = Actor { via: Via::Session, ..actor };
            let uniform = world.server.access.resolve(&session).await.unwrap();
            proptest::prop_assert!(uniform.sees_every_account(grant_ceiling(session.effective_scope())));
            Ok(())
        })?;
    }
}

/// One generated household: per account, its level for the token (`None`:
/// not named), and the token's `all` grant.
#[derive(Debug, Clone)]
struct Household {
    scope: Scope,
    all: Option<AccountLevel>,
    accounts: Vec<Option<AccountLevel>>,
}

fn household() -> impl proptest::strategy::Strategy<Value = Household> {
    (
        proptest::sample::select(vec![Scope::Read, Scope::Write]),
        proptest::option::of(level()),
        proptest::collection::vec(proptest::option::of(level()), 1..=4),
    )
        .prop_map(|(scope, all, accounts)| {
            let ceiling = grant_ceiling(scope);
            let cap = |level: Option<AccountLevel>| level.map(|level| level.min(ceiling));
            let mut all = cap(all);
            let accounts: Vec<Option<AccountLevel>> = accounts.into_iter().map(cap).collect();
            // A token with grants has at least one.
            if all.is_none() && accounts.iter().all(Option::is_none) {
                all = Some(AccountLevel::Balances);
            }
            Household {
                scope,
                all,
                accounts,
            }
        })
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]

    /// Every read endpoint, called with a token whose grants hide some
    /// accounts: no body holds a hidden account's id, name, provider
    /// account name, or transaction id or description, and none holds an
    /// unbound provider account without an `all` grant at `transactions`.
    #[test]
    fn no_endpoint_leaks_hidden_accounts(household in household()) {
        runtime().block_on(async {
            let world = world().await;
            let marker = uuid_marker();
            let mut grants = Vec::new();
            if let Some(level) = household.all {
                grants.push(grant(GrantTarget::All, level));
            }
            let mut accounts = Vec::new();
            let mut transactions = Vec::new();
            let mut hidden = Vec::new();
            let mut bindings = Vec::new();
            for (n, level) in household.accounts.iter().enumerate() {
                let name = format!("acct{n}{marker}");
                let account = world.account(&name).await;
                world.balance(account).await;
                let own = vec![
                    world.transaction(account, 1, &format!("txa{n}{marker}")).await,
                    world.transaction(account, 2, &format!("txb{n}{marker}")).await,
                ];
                if let Some(level) = level {
                    grants.push(on(account, *level));
                }
                if household.all.is_none() && level.is_none() {
                    hidden.push(s(account));
                    hidden.push(name);
                    hidden.push(format!("prov{n}{marker}"));
                    hidden.push(format!("txa{n}{marker}"));
                    hidden.push(format!("txb{n}{marker}"));
                    hidden.extend(own.iter().map(s));
                }
                bindings.push((format!("prov{n}{marker}"), account));
                accounts.push(account);
                transactions.extend(own);
            }
            let unbound = format!("unbound{marker}");
            if household.all.is_none_or(|all| all < AccountLevel::Transactions) {
                hidden.push(unbound.clone());
            }
            let mut names: Vec<(&str, Option<AccountId>)> = bindings
                .iter()
                .map(|(name, account)| (name.as_str(), Some(*account)))
                .collect();
            names.push((unbound.as_str(), None));
            let connection = world.connection(&names).await;
            let (_, token) = world.token(household.scope, grants).await;

            let mut uris = vec![
                "/api/v1/accounts".to_owned(),
                "/api/v1/accounts?include_closed=true".to_owned(),
                "/api/v1/transactions".to_owned(),
                "/api/v1/transactions?limit=1".to_owned(),
                "/api/v1/provider-accounts".to_owned(),
                "/api/v1/provider-accounts?unbound=true".to_owned(),
                "/api/v1/connections".to_owned(),
                format!("/api/v1/connections/{connection}/sync-runs"),
                "/api/v1/audit".to_owned(),
                "/api/v1/me".to_owned(),
            ];
            for account in &accounts {
                uris.push(format!("/api/v1/accounts/{account}"));
                uris.push(format!("/api/v1/accounts/{account}/balances"));
                uris.push(format!("/api/v1/transactions?account={account}"));
            }
            for transaction in &transactions {
                uris.push(format!("/api/v1/transactions/{transaction}"));
            }
            for uri in &uris {
                let (status, body) = world.send(Method::GET, uri, &token, None, None).await;
                proptest::prop_assert!(status != StatusCode::INTERNAL_SERVER_ERROR);
                for secret in &hidden {
                    proptest::prop_assert!(
                        !body.contains(secret.as_str()),
                        "a response reveals a hidden account"
                    );
                }
            }
            Ok(())
        })?;
    }
}

/// A suffix no generated id or fixed text contains, so a name is found only
/// where it was put.
fn uuid_marker() -> String {
    format!(
        "zq{}",
        AccountId::new().to_string().replace('-', "")[24..].to_owned()
    )
}

#[tokio::test]
async fn a_grant_that_no_longer_parses_fails_closed() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    for sql in [
        // A level the schema refuses.
        "INSERT INTO agent_token_grants (token_id, target, level) VALUES (?, 'all', 'admin')",
        // A target of the right length that is no account id.
        "INSERT INTO agent_token_grants (token_id, target, level) \
         VALUES (?, 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', 'write')",
    ] {
        let (id, token) = world
            .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
            .await;
        world
            .server
            .store
            .execute_unchecked_for_tests(sql, &s(id))
            .await
            .unwrap();
        let (status, text) = world
            .send(Method::GET, "/api/v1/accounts", &token, None, None)
            .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a corrupt grant refuses the request"
        );
        assert!(
            !text.contains(&s(a)) && !text.contains(&s(b)),
            "the refusal names no account"
        );
    }
}

#[tokio::test]
async fn grants_go_with_their_token() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    assert_eq!(world.server.store.token_grants(id).await.unwrap().len(), 1);
    world
        .server
        .store
        .execute_unchecked_for_tests("DELETE FROM agent_tokens WHERE id = ?", &s(id))
        .await
        .unwrap();
    let left = world.server.store.token_grants(id).await.unwrap();
    assert_eq!(left, Vec::<TokenGrant>::new(), "the grants are gone");
}
