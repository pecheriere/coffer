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
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::{AuditValue, FieldChange};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{
    AccountLevel, AccountScope, DenyReason, GrantError, GrantTarget, TokenGrant, authorize_account,
    grant_ceiling,
};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{Description, TransactionFlag, TransactionOrigin};
use coffer_server::auth::AuthError;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::CategoryGroupInput;
use coffer_server::ledger::transactions::NewManualTransactionInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::{
    AuditEvent, AuditFilter, ConnectionStatus, ConnectionStatusChange, ImportedFields,
    NewConnection, NewSource, NewTransaction, PageRequest, ProviderAccountUpsert,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use proptest::strategy::Strategy as _;
use proptest::test_runner::TestCaseError;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const LEVELS: [AccountLevel; 3] = [
    AccountLevel::Balances,
    AccountLevel::Transactions,
    AccountLevel::Write,
];

struct World {
    server: TestServer,
    fake: Arc<FakeProvider>,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    World {
        server,
        fake,
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

    /// Records `error` as the connection's last error.
    async fn fail_connection(&self, connection: ConnectionId, error: &str) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.update_connection_status(
            connection,
            ConnectionStatusChange {
                status: ConnectionStatus::Error,
                last_error: Some(error.to_owned()),
                attempted_at: Some(now),
                succeeded_at: None,
                at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// A transaction imported into `account` with one source record from the
    /// provider account of `connection` bound to it (the first one if none
    /// is). Returns its id and the source record's.
    async fn imported(
        &self,
        connection: ConnectionId,
        account: AccountId,
        description: &str,
    ) -> (TransactionId, String) {
        let now = self.server.clock.now();
        let accounts = self
            .server
            .store
            .list_provider_accounts(Some(connection))
            .await
            .unwrap();
        let provider_account = accounts
            .iter()
            .find(|provider_account| provider_account.account == Some(account))
            .unwrap_or(&accounts[0])
            .id;
        let posted_on = date(2026, 10, 3);
        let description = Description::from_provider(description);
        let mut tx = self.server.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Imported,
                account,
                posted_on,
                transacted_on: None,
                pending: false,
                amount_minor: -2500,
                currency: Currency::parse("USD").unwrap(),
                raw_description: description.clone(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: now,
            })
            .await
            .unwrap();
        let source = tx
            .insert_source(NewSource {
                connection,
                provider_account,
                provider_txn_id: format!("txn-{}", record.id),
                transaction: Some(record.id),
                fields: ImportedFields {
                    posted_on,
                    transacted_on: None,
                    pending: false,
                    amount_minor: -2500,
                    description,
                    payee: None,
                    memo: None,
                },
                amount_raw: "-25.00".to_owned(),
                seen_at: now,
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (record.id, s(source.id))
    }

    /// Flags `transaction` as a possible duplicate of `candidate`.
    async fn flag_duplicate(&self, transaction: TransactionId, candidate: TransactionId) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.set_flag(transaction, TransactionFlag::PossibleDuplicate, true)
            .await
            .unwrap();
        tx.add_duplicate_candidates(transaction, &[candidate])
            .await
            .unwrap();
        tx.commit().await.unwrap();
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

fn capped(scope: Scope, level: AccountLevel) -> AccountLevel {
    level.min(grant_ceiling(scope))
}

/// No `all` grant, at least one visible account and at least one hidden.
fn household_with_hidden_accounts() -> impl proptest::strategy::Strategy<Value = Household> {
    (
        proptest::sample::select(vec![Scope::Read, Scope::Write]),
        proptest::collection::vec(level(), 1..=3),
        1..=2_usize,
    )
        .prop_map(|(scope, visible, hidden)| Household {
            scope,
            all: None,
            accounts: visible
                .into_iter()
                .map(|level| Some(capped(scope, level)))
                .chain(std::iter::repeat_n(None, hidden))
                .collect(),
        })
}

/// An `all` grant, so no account is hidden; below `transactions` the
/// connection-wide data still is.
fn household_with_all_grant() -> impl proptest::strategy::Strategy<Value = Household> {
    (
        proptest::sample::select(vec![Scope::Read, Scope::Write]),
        level(),
        proptest::collection::vec(proptest::option::of(level()), 1..=3),
    )
        .prop_map(|(scope, all, accounts)| Household {
            scope,
            all: Some(capped(scope, all)),
            accounts: accounts
                .into_iter()
                .map(|level| level.map(|level| capped(scope, level)))
                .collect(),
        })
}

/// Every read endpoint, for these accounts and transactions.
fn read_uris(
    connection: ConnectionId,
    accounts: &[AccountId],
    transactions: &[TransactionId],
) -> Vec<String> {
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
    for account in accounts {
        uris.push(format!("/api/v1/accounts/{account}"));
        uris.push(format!("/api/v1/accounts/{account}/balances"));
        uris.push(format!("/api/v1/transactions?account={account}"));
    }
    for transaction in transactions {
        uris.push(format!("/api/v1/transactions/{transaction}"));
    }
    uris
}

/// Every read endpoint, called with a token holding `household`'s grants:
/// `/accounts` lists exactly the visible accounts, and no body holds a
/// hidden account's id, name, provider account name, transaction id or
/// description, or source id. Without an `all` grant at `transactions` no
/// body holds an unbound provider account or a connection's last error.
async fn check_no_endpoint_leaks(household: &Household) -> Result<(), TestCaseError> {
    let world = world().await;
    let marker = uuid_marker();
    let mut grants = Vec::new();
    if let Some(level) = household.all {
        grants.push(grant(GrantTarget::All, level));
    }
    let mut accounts = Vec::new();
    let mut visible = Vec::new();
    let mut transactions = Vec::new();
    let mut hidden = Vec::new();
    let mut bindings = Vec::new();
    for (n, level) in household.accounts.iter().enumerate() {
        let name = format!("acct{n}{marker}");
        let account = world.account(&name).await;
        world.balance(account).await;
        let own = vec![
            world
                .transaction(account, 1, &format!("txa{n}{marker}"))
                .await,
            world
                .transaction(account, 2, &format!("txb{n}{marker}"))
                .await,
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
        } else {
            visible.push(s(account));
        }
        bindings.push((format!("prov{n}{marker}"), account));
        accounts.push(account);
        transactions.extend(own);
    }
    let unbound = format!("unbound{marker}");
    let last_error = format!("lasterror{marker}");
    if household
        .all
        .is_none_or(|all| all < AccountLevel::Transactions)
    {
        hidden.push(unbound.clone());
        hidden.push(last_error.clone());
    }
    let mut names: Vec<(&str, Option<AccountId>)> = bindings
        .iter()
        .map(|(name, account)| (name.as_str(), Some(*account)))
        .collect();
    names.push((unbound.as_str(), None));
    let connection = world.connection(&names).await;
    world.fail_connection(connection, &last_error).await;

    // An imported transaction on each account, the last account's listed as
    // a duplicate of it, which a merge across accounts can leave behind.
    let last = accounts.len() - 1;
    let mut imported = Vec::new();
    for (n, account) in accounts.iter().enumerate() {
        let description = format!("imported{n}{marker}");
        let (transaction, source) = world.imported(connection, *account, &description).await;
        if household.all.is_none() && household.accounts[n].is_none() {
            hidden.push(description);
            hidden.push(s(transaction));
            hidden.push(source);
        }
        imported.push(transaction);
        transactions.push(transaction);
    }
    for transaction in &imported[..last] {
        world.flag_duplicate(*transaction, imported[last]).await;
    }
    world.flag_duplicate(imported[last], transactions[0]).await;

    let (_, token) = world.token(household.scope, grants).await;

    let (status, listed) = world.get("/api/v1/accounts", &token).await;
    proptest::prop_assert_eq!(status, StatusCode::OK);
    let mut listed = ids(&listed);
    listed.sort();
    visible.sort();
    proptest::prop_assert_eq!(listed, visible, "every visible account is listed");

    let uris = read_uris(connection, &accounts, &transactions);
    for uri in &uris {
        let (status, body) = world.send(Method::GET, uri, &token, None, None).await;
        proptest::prop_assert!(status != StatusCode::INTERNAL_SERVER_ERROR);
        for secret in &hidden {
            proptest::prop_assert!(
                !body.contains(secret.as_str()),
                "a response reveals something hidden"
            );
        }
    }
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]

    #[test]
    fn no_endpoint_leaks_hidden_accounts(household in household_with_hidden_accounts()) {
        runtime().block_on(check_no_endpoint_leaks(&household))?;
    }

    #[test]
    fn no_endpoint_leaks_with_an_all_grant(household in household_with_all_grant()) {
        runtime().block_on(check_no_endpoint_leaks(&household))?;
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

impl World {
    /// Member "bob", signed in with a passkey.
    async fn bob(&self) -> Actor {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let member = self
            .server
            .authenticator()
            .enroll(&invite, "bob", "Bob")
            .await;
        Actor {
            member,
            role: Role::Member,
            via: Via::Session,
        }
    }

    /// The audit events of `token` with `action`, newest first.
    async fn token_events(&self, token: AgentTokenId, action: &str) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_id: Some(s(token)),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }
}

/// The `grants` change an audit event recorded.
fn grants_change(event: &AuditEvent) -> &FieldChange {
    event
        .changes
        .iter()
        .find_map(|(field, change)| (field == "grants").then_some(change))
        .unwrap()
}

#[tokio::test]
async fn grants_change_audited_and_session_only() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;
    let (id, secret) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await;

    // An agent cannot change grants, not even its own token's; through the
    // router its bearer token gets it no page route either.
    let agent = world
        .server
        .agent_tokens
        .resolve(secret.expose_secret())
        .await
        .unwrap();
    let refused = world
        .server
        .agent_tokens
        .replace_grants(
            &agent,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(AuthError::Denied(denied)) if denied.reason == DenyReason::SessionRequired
        ),
        "an agent is refused"
    );
    let (status, _) = world
        .send(
            Method::POST,
            &format!("/account/tokens/{id}/grants"),
            &secret,
            None,
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a bearer token opens no page"
    );
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [grant(GrantTarget::All, AccountLevel::Balances)],
        "refused attempts change nothing"
    );
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);

    // A session can, and the next request of the token obeys.
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![on(b, AccountLevel::Write), on(a, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    let (status, _) = world
        .get(&format!("/api/v1/accounts/{a}/balances"), &secret)
        .await;
    assert_eq!(status, StatusCode::OK);
    let new = world.account("New").await;
    let (status, _) = world.get(&format!("/api/v1/accounts/{new}"), &secret).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the all grant is gone");

    let events = world.token_events(id, "token.grants").await;
    assert_eq!(events.len(), 1, "one audit entry");
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    let level = |account: AccountId| if account == a { "balances" } else { "write" };
    assert_eq!(
        *grants_change(&events[0]),
        FieldChange {
            before: AuditValue::from("all=balances"),
            after: AuditValue::from(
                format!("{first}={},{second}={}", level(first), level(second)).as_str()
            ),
        },
        "the entry holds both canonical forms"
    );
}

#[tokio::test]
async fn granting_a_uniform_token_records_no_before() {
    let world = world().await;
    let (id, _) = world.token(Scope::Read, Vec::new()).await;
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![grant(GrantTarget::All, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    let events = world.token_events(id, "token.grants").await;
    assert_eq!(
        *grants_change(&events[0]),
        FieldChange {
            before: AuditValue::Null,
            after: AuditValue::from("all=balances"),
        },
        "a token without grants has nothing before"
    );
}

/// No grant rows means uniform access, so emptying the list would silently
/// widen a restricted token to everything its scope allows.
#[tokio::test]
async fn emptying_grants_is_refused_and_never_widens() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;
    let (id, secret) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Balances)])
        .await;

    let refused = world
        .server
        .agent_tokens
        .replace_grants(&world.alice, id, Vec::new())
        .await;
    assert!(
        matches!(refused, Err(AuthError::InvalidGrants(GrantError::Empty))),
        "an empty list is refused"
    );
    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [on(a, AccountLevel::Balances)],
        "the grants stay"
    );
    let (status, _) = world.get(&format!("/api/v1/accounts/{b}"), &secret).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "still hidden");
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);

    // Widening stays possible, and it is written down: an explicit `all`.
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
    let (status, _) = world.get(&format!("/api/v1/accounts/{b}"), &secret).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn replaced_grants_are_validated_like_new_ones() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    let replace = async |grants: Vec<TokenGrant>| {
        world
            .server
            .agent_tokens
            .replace_grants(&world.alice, id, grants)
            .await
    };

    assert!(matches!(
        replace(vec![on(a, AccountLevel::Write)]).await,
        Err(AuthError::InvalidGrants(GrantError::AboveScope))
    ));
    assert!(matches!(
        replace(vec![
            on(a, AccountLevel::Balances),
            on(a, AccountLevel::Transactions)
        ])
        .await,
        Err(AuthError::InvalidGrants(GrantError::DuplicateTarget))
    ));
    let many = (0..=coffer_domain::policy::MAX_GRANTS)
        .map(|_| on(AccountId::new(), AccountLevel::Balances))
        .collect();
    assert!(matches!(
        replace(many).await,
        Err(AuthError::InvalidGrants(GrantError::TooMany))
    ));
    assert!(matches!(
        replace(vec![on(AccountId::new(), AccountLevel::Balances)]).await,
        Err(AuthError::UnknownGrantedAccount)
    ));

    assert_eq!(
        world.server.store.token_grants(id).await.unwrap(),
        [on(a, AccountLevel::Balances)],
        "nothing changed"
    );
    assert_eq!(world.token_events(id, "token.grants").await.len(), 0);
}

#[tokio::test]
async fn another_members_token_is_hidden_unless_managing_members() {
    let world = world().await;
    let a = world.account("Checking").await;
    let bob = world.bob().await;
    let (alice_token, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    let (bob_token, _) = world
        .server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("bobs").unwrap(),
            Scope::Read,
            None,
            vec![on(a, AccountLevel::Balances)],
        )
        .await
        .map(|(record, secret)| (record.id, secret))
        .unwrap();
    let wide = vec![grant(GrantTarget::All, AccountLevel::Transactions)];

    // Bob gets what a missing token gets, whether or not his list is valid.
    for grants in [wide.clone(), Vec::new()] {
        for token in [alice_token, AgentTokenId::new()] {
            assert!(
                matches!(
                    world
                        .server
                        .agent_tokens
                        .replace_grants(&bob, token, grants.clone())
                        .await,
                    Err(AuthError::NotFound)
                ),
                "hidden like a missing token"
            );
        }
    }
    assert_eq!(
        world.server.store.token_grants(alice_token).await.unwrap(),
        [on(a, AccountLevel::Balances)]
    );

    // His own token and, as an admin, anyone's.
    world
        .server
        .agent_tokens
        .replace_grants(&bob, bob_token, vec![on(a, AccountLevel::Transactions)])
        .await
        .unwrap();
    world
        .server
        .agent_tokens
        .replace_grants(&world.alice, bob_token, wide.clone())
        .await
        .unwrap();
    assert_eq!(
        world.server.store.token_grants(bob_token).await.unwrap(),
        wide
    );
}

#[tokio::test]
async fn revoked_and_expired_tokens_keep_their_grants() {
    let world = world().await;
    let a = world.account("Checking").await;
    let wide = vec![grant(GrantTarget::All, AccountLevel::Balances)];

    let (revoked, _) = world
        .token(Scope::Read, vec![on(a, AccountLevel::Balances)])
        .await;
    world
        .server
        .agent_tokens
        .revoke(&world.alice, revoked)
        .await
        .unwrap();

    // `create` always makes a new token: one born expired stores its grants
    // and can never be used, so grants cannot revive anything.
    let past = world
        .server
        .clock
        .now()
        .checked_sub(jiff::SignedDuration::from_hours(1))
        .unwrap();
    let (expired, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("late").unwrap(),
            Scope::Read,
            Some(past),
            vec![on(a, AccountLevel::Balances)],
        )
        .await
        .unwrap();
    assert!(matches!(
        world
            .server
            .agent_tokens
            .resolve(secret.expose_secret())
            .await,
        Err(AuthError::Unauthenticated)
    ));

    for token in [revoked, expired.id] {
        assert!(
            matches!(
                world
                    .server
                    .agent_tokens
                    .replace_grants(&world.alice, token, wide.clone())
                    .await,
                Err(AuthError::TokenInactive)
            ),
            "a token that no longer works takes no new grants"
        );
        assert_eq!(
            world.server.store.token_grants(token).await.unwrap(),
            [on(a, AccountLevel::Balances)]
        );
        assert_eq!(world.token_events(token, "token.grants").await.len(), 0);
    }
}

#[tokio::test]
async fn replacing_grants_with_the_same_ones_records_nothing() {
    let world = world().await;
    let a = world.account("Checking").await;
    let (id, _) = world
        .token(
            Scope::Write,
            vec![
                on(a, AccountLevel::Write),
                grant(GrantTarget::All, AccountLevel::Balances),
            ],
        )
        .await;
    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![
                grant(GrantTarget::All, AccountLevel::Balances),
                on(a, AccountLevel::Write),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        world.token_events(id, "token.grants").await.len(),
        0,
        "the same grants in another order change nothing"
    );
}

#[tokio::test]
async fn me_reports_access() {
    let world = world().await;
    let a = world.account("Checking").await;
    let b = world.account("Savings").await;

    // Uniform: everything at the scope's level, no account listed.
    let (_, uniform) = world.token(Scope::Read, Vec::new()).await;
    let (status, body) = world.get("/api/v1/me", &uniform).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["access"],
        json!({"ceiling": "transactions", "all": "transactions", "accounts": []})
    );

    // Restricted: only what the grants name.
    let (_, restricted) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Balances)])
        .await;
    let (_, body) = world.get("/api/v1/me", &restricted).await;
    assert_eq!(
        body["access"],
        json!({
            "ceiling": "write",
            "all": null,
            "accounts": [{"account": s(a), "level": "balances"}],
        })
    );

    // With an `all` grant, a listed account shows the level it really has.
    let (_, both) = world
        .token(
            Scope::Write,
            vec![
                grant(GrantTarget::All, AccountLevel::Transactions),
                on(b, AccountLevel::Write),
                on(a, AccountLevel::Balances),
            ],
        )
        .await;
    let (_, body) = world.get("/api/v1/me", &both).await;
    assert_eq!(body["access"]["all"], "transactions");
    let levels: BTreeMap<String, String> = body["access"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|grant| {
            (
                grant["account"].as_str().unwrap().to_owned(),
                grant["level"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        levels,
        BTreeMap::from([
            (s(a), "transactions".to_owned()),
            (s(b), "write".to_owned()),
        ])
    );
}

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

/// A merge across accounts can leave a transaction listing a candidate in
/// another account: a token that cannot see that account is not told its id.
#[tokio::test]
async fn duplicate_candidates_in_hidden_accounts_are_not_named() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let flagged = world.transaction(a, 1, "Coffee").await;
    let same_account = world.transaction(a, 2, "Coffee again").await;
    let hidden = world.transaction(b, 1, "Coffee elsewhere").await;
    world.flag_duplicate(flagged, same_account).await;
    world.flag_duplicate(flagged, hidden).await;

    let (_, restricted) = world
        .token(Scope::Write, vec![on(a, AccountLevel::Write)])
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

    // One that sees both accounts is told both.
    let (_, wide) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (_, got) = world.get(&uri, &wide).await;
    assert_eq!(got["duplicate_candidates"].as_array().unwrap().len(), 2);
}
