//! Connections, sync rate limits and provider account binding (spec §6.2,
//! §6.11): credentials sealed before anything is stored, removal that
//! serializes with a running sync, limits that refuse with the right
//! `Retry-After`, and binding that turns held records into transactions.
//! Providers are scripted with the testkit's `FakeProvider`.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, ProviderAccountId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, AccountScope, Action, Denied, DenyReason,
};
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::SyncError;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{
    ConnectInput, FetchResult, ProviderAccountData, ProviderError, ProviderKind,
    ProviderTransaction,
};
use coffer_server::testkit::{FakeProvider, TestServer, captured_log_text};
use coffer_server::vault::SealedCredentials;
use coffer_store::{
    AuditEvent, AuditFilter, ConnectionStatus, ConnectionStatusChange, NewSyncRun, PageRequest,
    ProviderAccountRecord, TransactionFilter, TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use jiff::{SignedDuration, Timestamp};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// The credentials of every test connection. Recognisable, so a leak shows
/// up in a text search.
const PLAINTEXT: &str = "https://user:SECRET-CREDENTIAL-MARKER@bridge.example/simplefin";
const SECRET_PART: &str = "SECRET-CREDENTIAL-MARKER";

struct World {
    server: TestServer,
    fake: Arc<FakeProvider>,
    alice: Actor,
    connection: ConnectionId,
}

/// Admin "alice" with a connection seeded under her name.
async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let connection = server
        .seed_connection(ProviderKind::SimpleFin, "Family bank", member, PLAINTEXT)
        .await;
    World {
        server,
        fake,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
        connection,
    }
}

impl World {
    fn now(&self) -> Timestamp {
        self.server.clock.now()
    }

    /// Fetches `accounts` from the provider in a run queued directly, so no
    /// rate limit applies.
    async fn sync_once(&self, accounts: Vec<ProviderAccountData>) {
        self.fake.push(Ok(FetchResult {
            accounts,
            connection_errors: Vec::new(),
            cursor: None,
        }));
        let mut tx = self.server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: self.connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.alice.member, None)),
                queued_at: self.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.server.sync.run(run.id).await.unwrap();
    }

    async fn account(&self, name: &str, currency: &str) -> AccountId {
        self.server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Checking,
                    currency: Currency::parse(currency).unwrap(),
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

    async fn provider_accounts(&self) -> Vec<ProviderAccountRecord> {
        self.server
            .store
            .list_provider_accounts(Some(self.connection))
            .await
            .unwrap()
    }

    async fn provider_account(&self, provider_account_id: &str) -> ProviderAccountRecord {
        self.provider_accounts()
            .await
            .into_iter()
            .find(|record| record.provider_account_id == provider_account_id)
            .unwrap()
    }

    async fn transactions(&self, account: AccountId) -> Vec<TransactionRecord> {
        self.server
            .store
            .list_transactions(
                &TransactionFilter {
                    scope: AccountScope::All,
                    accounts: vec![account],
                    from: None,
                    to: None,
                    review: None,
                    assignee: None,
                    categories: Vec::new(),
                    uncategorized: false,
                    pending: None,
                    flag: None,
                    min_minor: None,
                    max_minor: None,
                    text: None,
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
    }

    async fn audit(&self) -> Vec<AuditEvent> {
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
    }

    async fn audit_actions(&self, action: &str) -> Vec<AuditEvent> {
        self.audit()
            .await
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }

    async fn token(&self, scope: Scope) -> SecretToken {
        let (_, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
            )
            .await
            .unwrap();
        secret
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
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
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn sealed(&self) -> Option<Vec<u8>> {
        self.server
            .store
            .get_connection(self.connection)
            .await
            .unwrap()
            .credentials
    }

    /// Whether `sealed` opens to `expected`; never prints either.
    fn opens_to(&self, id: ConnectionId, sealed: Vec<u8>, expected: &str) -> bool {
        self.server
            .vault
            .open(id, &SealedCredentials::from(sealed))
            .is_ok_and(|opened| opened.expose_secret() == expected)
    }
}

fn setup_token(text: &str) -> ConnectInput {
    ConnectInput::SimpleFinSetupToken(SecretString::from(text))
}

fn txn(provider_id: &str, value: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: provider_id.into(),
        posted_on: date(2027, 1, 10),
        transacted_on: None,
        amount: DecimalAmount::parse(value).unwrap(),
        description: "COFFEE SHOP PURCHASE".into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

fn reported(
    provider_account_id: &str,
    currency: &str,
    kind_hint: Option<AccountKind>,
    transactions: Vec<ProviderTransaction>,
) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: provider_account_id.into(),
        name: "Everyday savings".into(),
        institution: Some("Example Bank".into()),
        currency: currency.into(),
        kind_hint,
        balance: None,
        transactions,
        removed: Vec::new(),
        coverage: Coverage::Delta,
        error: None,
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Whether `err` is a denial of `action` for `reason`.
fn denied(err: &SyncError, action: Action, reason: DenyReason) -> bool {
    matches!(err, SyncError::Denied(d) if *d == Denied { action, reason })
}

#[tokio::test]
async fn bind_requires_same_currency() {
    let world = world().await;
    world
        .sync_once(vec![
            reported("acct-usd", "USD", None, vec![txn("t-1", "-4.50")]),
            reported("acct-odd", "DOGE", None, Vec::new()),
        ])
        .await;
    let usd = world.provider_account("acct-usd").await;
    let odd = world.provider_account("acct-odd").await;
    let euros = world.account("Euro account", "EUR").await;

    let err = world
        .server
        .sync
        .bind(&world.alice, usd.id, euros)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::CurrencyMismatch));
    let err = world
        .server
        .sync
        .bind(&world.alice, odd.id, euros)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::CurrencyMismatch));

    assert_eq!(world.provider_account("acct-usd").await.account, None);
    assert_eq!(world.transactions(euros).await.len(), 0);
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 0);
}

#[tokio::test]
async fn bind_applies_held_records() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            None,
            vec![txn("t-1", "-4.50"), txn("t-2", "1200.00")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    assert_eq!(
        world.transactions(checking).await.len(),
        0,
        "records of an unbound provider account are held"
    );

    let reader = world.token(Scope::Read).await;
    let (status, unbound) = world
        .call(
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &reader,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unbound["items"][0]["id"], json!(held.id.to_string()));
    assert_eq!(unbound["items"][0]["currency"], json!("USD"));

    let bound = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();
    assert_eq!(bound.account, Some(checking));

    let mut amounts: Vec<i64> = world
        .transactions(checking)
        .await
        .iter()
        .map(|transaction| transaction.amount_minor)
        .collect();
    amounts.sort_unstable();
    assert_eq!(amounts, [-450, 120_000]);

    let binds = world.audit_actions("provider_account.bind").await;
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].entity_id, held.id.to_string());
    assert_eq!(
        binds[0].actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: None
        }
    );

    let (_, unbound) = world
        .call(
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &reader,
            None,
        )
        .await;
    assert_eq!(unbound["items"], json!([]));
    let (_, all) = world
        .call(Method::GET, "/api/v1/provider-accounts", &reader, None)
        .await;
    assert_eq!(all["items"][0]["account"], json!(checking.to_string()));

    let again = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap_err();
    assert!(matches!(again, SyncError::Conflict(_)));
}

#[tokio::test]
async fn create_account_and_bind_via_api() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Savings),
            vec![txn("t-1", "25.00")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let writer = world.token(Scope::Write).await;

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(linked["account"]["kind"], json!("savings"));
    assert_eq!(linked["account"]["name"], json!("Everyday savings"));
    assert_eq!(linked["account"]["currency"], json!("USD"));
    assert_eq!(linked["account"]["source"], json!("linked"));
    assert_eq!(linked["account"]["liquidity"], json!("cash"));
    assert_eq!(
        linked["provider_account"]["account"],
        linked["account"]["id"]
    );
    let account: AccountId = linked["account"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(world.transactions(account).await.len(), 1);

    // A manual account created through the API, for the audit's shape.
    let (status, manual) = world
        .call(
            Method::POST,
            "/api/v1/accounts",
            &writer,
            Some(json!({"name": "Manual", "kind": "savings", "currency": "USD"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let creates = world.audit_actions("account.create").await;
    let linked_create = creates
        .iter()
        .find(|event| event.entity_id == account.to_string())
        .unwrap();
    let manual_create = creates
        .iter()
        .find(|event| Some(event.entity_id.as_str()) == manual["id"].as_str())
        .unwrap();
    let fields = |event: &AuditEvent| -> Vec<String> {
        event.changes.iter().map(|(f, _)| f.to_owned()).collect()
    };
    assert_eq!(fields(linked_create), fields(manual_create));
    assert_eq!(linked_create.entity_type, "account");
    assert_eq!(linked_create.actor, manual_create.actor);
    assert!(matches!(
        linked_create.actor,
        AuditActor::Member { agent: Some(_), .. }
    ));
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 1);

    // Bound already: refused, and no second account is created.
    let accounts_before = world
        .server
        .store
        .list_accounts(&AccountScope::All, true)
        .await
        .unwrap()
        .len();
    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("conflict"));
    let accounts_after = world
        .server
        .store
        .list_accounts(&AccountScope::All, true)
        .await
        .unwrap()
        .len();
    assert_eq!(accounts_after, accounts_before);
}

#[tokio::test]
async fn create_account_needs_a_kind_and_a_known_currency() {
    let world = world().await;
    world
        .sync_once(vec![
            reported("acct-plain", "USD", None, Vec::new()),
            reported("acct-odd", "DOGE", Some(AccountKind::Checking), Vec::new()),
        ])
        .await;
    let plain = world.provider_account("acct-plain").await;
    let odd = world.provider_account("acct-odd").await;
    let writer = world.token(Scope::Write).await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", plain.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("invalid"));
    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", odd.id),
            &writer,
            Some(json!({"name": "Odd"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("currency_mismatch"));

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", plain.id),
            &writer,
            Some(json!({"name": "Joint checking", "kind": "checking"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(linked["account"]["name"], json!("Joint checking"));
    assert_eq!(linked["account"]["kind"], json!("checking"));
    assert_eq!(world.audit_actions("account.create").await.len(), 1);
}

#[tokio::test]
async fn read_token_cannot_bind() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Checking),
            vec![txn("t-1", "-4.50")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    let reader = world.token(Scope::Read).await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/bind", held.id),
            &reader,
            Some(json!({"account": checking})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("forbidden"));
    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &reader,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    assert_eq!(world.provider_account("acct-1").await.account, None);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 0);

    // The same request with a write token binds.
    let writer = world.token(Scope::Write).await;
    let (status, bound) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/bind", held.id),
            &writer,
            Some(json!({"account": checking})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bound["account"], json!(checking.to_string()));
}

#[tokio::test]
async fn bind_needs_write_on_account() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    let reader = Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Read,
        },
    };
    assert!(matches!(
        world.server.access.resolve(&reader).await,
        Ok(AccountAccess::Uniform(AccountLevel::Transactions))
    ));

    let err = world
        .server
        .sync
        .bind(&reader, held.id, checking)
        .await
        .unwrap_err();
    assert!(denied(
        &err,
        Action::AccessAccount {
            needed: AccountLevel::Write
        },
        DenyReason::AccountLevel {
            needed: AccountLevel::Write
        },
    ));
    assert_eq!(world.provider_account("acct-1").await.account, None);
}

#[tokio::test]
async fn rate_limit_refuses_with_retry_after() {
    let world = world().await;
    let sync = &world.server.sync;
    let hour = SignedDuration::from_hours(1);

    // 16 manual syncs, an hour apart: the daily budget.
    for _ in 0..16 {
        let run = sync
            .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
            .await
            .unwrap();
        sync.run(run.id).await.unwrap();
        world.server.clock.advance(hour);
    }
    // The 17th within 24 h waits until the first leaves the window.
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_hours(8)
    ));
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_hours(8)
    ));
    world.server.clock.advance(SignedDuration::from_hours(8));
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(world.audit_actions("sync.request").await.len(), 17);
}

#[tokio::test]
async fn manual_and_scheduled_syncs_keep_their_minimum_interval() {
    let world = world().await;
    let sync = &world.server.sync;
    let run = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    assert_eq!(run.trigger, SyncTrigger::Manual);
    assert_eq!(run.status, SyncRunStatus::Queued);
    assert_eq!(
        run.requested_by.as_ref().and_then(|by| by.member),
        Some(world.alice.member)
    );
    sync.run(run.id).await.unwrap();

    world.server.clock.advance(SignedDuration::from_mins(30));
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_mins(30)
    ));
    world.server.clock.advance(SignedDuration::from_mins(60));
    // An hour and a half after the last start: a manual sync may run, a
    // scheduled one waits for two hours.
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SyncError::RateLimited { retry_after } if retry_after == SignedDuration::from_mins(30)
    ));
    let scheduled = {
        world.server.clock.advance(SignedDuration::from_mins(30));
        sync.request_sync(None, world.connection, SyncTrigger::Schedule)
            .await
            .unwrap()
    };
    assert_eq!(scheduled.requested_by, None);
    // Only manual requests are audited.
    assert_eq!(world.audit_actions("sync.request").await.len(), 1);

    // Any other pairing of requester and trigger is refused.
    for (actor, trigger) in [
        (None, SyncTrigger::Manual),
        (Some(&world.alice), SyncTrigger::Schedule),
        (Some(&world.alice), SyncTrigger::Initial),
    ] {
        let err = sync
            .request_sync(actor, world.connection, trigger)
            .await
            .unwrap_err();
        assert!(matches!(err, SyncError::Invalid(_)));
    }
}

#[tokio::test]
async fn second_request_while_queued_refused() {
    let world = world().await;
    let sync = &world.server.sync;
    sync.request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    let err = sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::InProgress));
    let problem = coffer_server::http::problem::Problem::from(err);
    assert_eq!(problem.status, StatusCode::CONFLICT);
    assert_eq!(
        problem.code,
        coffer_server::http::problem::ProblemCode::SyncInProgress
    );
    let err = sync
        .request_sync(None, world.connection, SyncTrigger::Schedule)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::InProgress));
    assert_eq!(world.audit_actions("sync.request").await.len(), 1);
}

#[tokio::test]
async fn credentials_never_stored_in_clear() {
    let world = world().await;
    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse("Second bank").unwrap(),
            setup_token(PLAINTEXT),
            None,
        )
        .await
        .unwrap();
    assert_eq!(view.provider, "simplefin");
    assert_eq!(view.label, "Second bank");
    assert_eq!(view.owner, world.alice.member);
    assert_eq!(view.accounts.len(), 0);

    let record = world.server.store.get_connection(view.id).await.unwrap();
    let sealed = record.credentials.unwrap();
    assert!(!contains(&sealed, SECRET_PART));
    // Positive control: the sealed bytes do hold the credentials.
    assert!(world.opens_to(view.id, sealed, PLAINTEXT));
    assert!(!serde_json::to_string(&view).unwrap().contains(SECRET_PART));

    for event in world.audit().await {
        assert!(
            !serde_json::to_string(&event.changes)
                .unwrap()
                .contains(SECRET_PART)
        );
    }
    let creates = world.audit_actions("connection.create").await;
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].entity_id, view.id.to_string());
    let fields: Vec<&str> = creates[0].changes.iter().map(|(f, _)| f).collect();
    assert_eq!(fields, ["label", "provider"]);

    let log = captured_log_text();
    assert!(log.contains(&view.id.to_string()));
    assert!(!log.contains(SECRET_PART));

    let runs = world
        .server
        .store
        .list_sync_runs(view.id, &PageRequest::default())
        .await
        .unwrap()
        .items;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger, SyncTrigger::Initial);
    assert_eq!(runs[0].status, SyncRunStatus::Queued);
}

#[tokio::test]
async fn list_connections_shows_schedule_and_visible_accounts() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let attempted = world.now();
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.id, world.connection);
    assert_eq!(view.last_attempt_at, Some(attempted));
    let interval = world
        .server
        .config
        .sync_interval
        .max(SignedDuration::from_hours(2));
    assert_eq!(
        view.next_sync_at,
        Some(attempted.checked_add(interval).unwrap())
    );
    assert_eq!(view.accounts.len(), 1);
    assert_eq!(view.accounts[0].name, "Everyday savings");
    assert_eq!(
        view.accounts[0].institution.as_deref(),
        Some("Example Bank")
    );

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let views = world
        .server
        .sync
        .list_connections(&world.alice)
        .await
        .unwrap();
    assert_eq!(views[0].next_sync_at, None);
    assert_eq!(views[0].accounts.len(), 1, "provider accounts stay");
}

#[tokio::test]
async fn reconnect_reseals_and_keeps_bindings() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            None,
            vec![txn("t-1", "-4.50")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();
    let before = world.sealed().await.unwrap();
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        world.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::ReauthRequired,
            last_error: Some("the provider refused the credentials".into()),
            attempted_at: None,
            succeeded_at: None,
            at: world.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    world.server.clock.advance(SignedDuration::from_hours(1));

    let renewed = "https://user:RENEWED-CREDENTIAL@bridge.example/simplefin";
    let view = world
        .server
        .sync
        .reconnect(&world.alice, world.connection, setup_token(renewed))
        .await
        .unwrap();
    assert_eq!(view.id, world.connection);
    assert!(matches!(
        view.status,
        coffer_server::sync::connections::ConnectionState::Active
    ));
    assert_eq!(view.last_error, None);
    assert_eq!(view.accounts.len(), 1);
    assert_eq!(view.accounts[0].account, Some(checking));

    let after = world.sealed().await.unwrap();
    assert_ne!(after, before);
    assert!(!contains(&after, "RENEWED-CREDENTIAL"));
    assert!(world.opens_to(world.connection, after, renewed));
    assert_eq!(
        world.provider_account("acct-1").await.account,
        Some(checking)
    );
    assert_eq!(world.transactions(checking).await.len(), 1);

    let reconnects = world.audit_actions("connection.reconnect").await;
    assert_eq!(reconnects.len(), 1);
    assert!(
        !serde_json::to_string(&reconnects[0].changes)
            .unwrap()
            .contains("RENEWED-CREDENTIAL")
    );
    let runs = world
        .server
        .store
        .list_sync_runs(world.connection, &PageRequest::default())
        .await
        .unwrap()
        .items;
    assert_eq!(runs[0].trigger, SyncTrigger::Manual);
    assert_eq!(runs[0].status, SyncRunStatus::Queued);

    // The next sync uses the new credentials.
    world.fake.push(Ok(FetchResult {
        accounts: Vec::new(),
        connection_errors: Vec::new(),
        cursor: None,
    }));
    world.server.sync.run(runs[0].id).await.unwrap();
    let fetches = world.fake.requests();
    let used_renewed = fetches.last().unwrap().credentials.expose_secret() == renewed;
    assert!(used_renewed);
}

#[tokio::test]
async fn remove_fails_cleanly_when_disconnect_fails() {
    let world = world().await;
    let before = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    world
        .fake
        .fail_next_disconnect(ProviderError::Unavailable("bridge down".into()));

    let err = world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Provider(_)));
    let problem = coffer_server::http::problem::Problem::from(err);
    assert_eq!(problem.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        world
            .server
            .store
            .get_connection(world.connection)
            .await
            .unwrap(),
        before
    );
    assert_eq!(world.audit_actions("connection.remove").await.len(), 0);

    // Once the provider answers, the removal goes through.
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let removed = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert_eq!(removed.credentials, None);
    let disconnects = world.fake.disconnects();
    assert_eq!(disconnects.len(), 2);
    let gave_up_stored = disconnects[1].expose_secret() == PLAINTEXT;
    assert!(gave_up_stored);
    assert_eq!(world.audit_actions("connection.remove").await.len(), 1);

    let err = world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)));
    let err = world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)));
}

#[tokio::test]
async fn member_cannot_remove_or_reconnect_others_connection() {
    let world = world().await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let bob_member = world
        .server
        .authenticator()
        .enroll(&invite, "bob", "Bob")
        .await;
    let bob = Actor {
        member: bob_member,
        role: Role::Member,
        via: Via::Session,
    };
    let before = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    let needs_admin = DenyReason::InsufficientScope {
        needed: Scope::Admin,
    };
    let action = Action::RemoveConnection {
        owner: world.alice.member,
    };

    let err = world
        .server
        .sync
        .reconnect(
            &bob,
            world.connection,
            setup_token("https://other@bridge.example"),
        )
        .await
        .unwrap_err();
    assert!(denied(&err, action, needs_admin));
    let err = world
        .server
        .sync
        .remove_connection(&bob, world.connection)
        .await
        .unwrap_err();
    assert!(denied(&err, action, needs_admin));
    assert_eq!(
        world
            .server
            .store
            .get_connection(world.connection)
            .await
            .unwrap(),
        before
    );
    assert_eq!(world.fake.disconnects().len(), 0);

    // Bob may remove a connection he added.
    let own = world
        .server
        .seed_connection(ProviderKind::SimpleFin, "Bob's bank", bob_member, PLAINTEXT)
        .await;
    world
        .server
        .sync
        .remove_connection(&bob, own)
        .await
        .unwrap();
    assert_eq!(
        world.server.store.get_connection(own).await.unwrap().status,
        ConnectionStatus::Removed
    );
}

/// A connection removed while a run is fetching gets nothing written: the
/// apply step re-checks the connection in the transaction that writes.
#[tokio::test]
async fn removal_during_a_sync_writes_nothing() {
    let world = world().await;
    let gate = world.fake.hold_next_fetch();
    world.fake.push(Ok(FetchResult {
        accounts: vec![reported("acct-1", "USD", None, vec![txn("t-1", "-4.50")])],
        connection_errors: Vec::new(),
        cursor: None,
    }));
    let run = world
        .server
        .sync
        .request_sync(Some(&world.alice), world.connection, SyncTrigger::Manual)
        .await
        .unwrap();
    let sync = Arc::clone(&world.server.sync);
    let running = tokio::spawn(async move { sync.run(run.id).await });

    gate.entered().await;
    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    gate.release();
    let finished = running.await.unwrap().unwrap();

    assert_eq!(finished.status, SyncRunStatus::Failed);
    assert_eq!(
        finished.detail.as_deref(),
        Some("the connection is removed")
    );
    assert_eq!(world.provider_accounts().await.len(), 0);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Removed);
    assert_eq!(connection.last_success_at, None);
}

#[tokio::test]
async fn bind_refuses_unknown_and_removed() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;

    let err = world
        .server
        .sync
        .bind(&world.alice, ProviderAccountId::new(), checking)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::NotFound));
    let err = world
        .server
        .sync
        .bind(&world.alice, held.id, AccountId::new())
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::NotFound));

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let err = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Conflict(_)));
    assert_eq!(world.provider_account("acct-1").await.account, None);
}
