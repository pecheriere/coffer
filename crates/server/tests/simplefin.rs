//! The `SimpleFin` provider against a loopback mock of the bridge: the
//! claim and its SSRF checks, the accounts request, parsing of recorded
//! generic fixtures, error mapping, and credentials that never reach the
//! database in clear, an API response, the audit, a run's detail or a log.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId};
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::http::router;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::connections::NewLinkedAccountInput;
use coffer_server::sync::provider::{ConnectInput, ProviderKind};
use coffer_server::testkit::{MockUpstream, TestServer, captured_log_text};
use coffer_server::vault::SealedCredentials;
use coffer_store::{
    AuditFilter, ConnectionStatus, NewSyncRun, PageRequest, ProviderAccountRecord, SyncRunRecord,
    TransactionFilter, TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use jiff::tz::TimeZone;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ACCOUNTS_FIRST: &str = include_str!("fixtures/simplefin/accounts_first.json");
const ACCOUNTS_POSTED: &str = include_str!("fixtures/simplefin/accounts_posted.json");
const ACCOUNTS_ERRORS: &str = include_str!("fixtures/simplefin/accounts_errors.json");

/// The access URL's user and password, recognisable in a text search. The
/// password holds a percent-encoded `/`, sent decoded in basic auth.
const USER: &str = "sfuser-7f3a";
const PASSWORD_ENCODED: &str = "SFPASS-MARKER%2F42";
const PASSWORD: &str = "SFPASS-MARKER/42";
const PASSWORD_MARKER: &str = "SFPASS-MARKER";
/// A setup token that is no valid token, recognisable in a text search.
const BOGUS_TOKEN: &str = "SECRETTOKEN-not-a-real-setup-token-0000";

/// One scripted answer of the mock bridge. `{host}` in the body or the
/// location becomes the mock's `127.0.0.1:<port>`.
#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    location: Option<String>,
}

impl Reply {
    fn ok(body: &str) -> Self {
        Self::status(StatusCode::OK, body)
    }

    fn status(status: StatusCode, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
            location: None,
        }
    }
}

/// The answers the mock bridge gives, in order, per endpoint.
#[derive(Default)]
struct Script {
    claims: Mutex<VecDeque<Reply>>,
    accounts: Mutex<VecDeque<Reply>>,
}

impl Script {
    fn claim(&self, reply: Reply) {
        self.claims.lock().unwrap().push_back(reply);
    }

    fn accounts(&self, reply: Reply) {
        self.accounts.lock().unwrap().push_back(reply);
    }
}

fn answer(queue: &Mutex<VecDeque<Reply>>, headers: &HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let Some(reply) = queue.lock().unwrap().pop_front() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "unscripted").into_response();
    };
    let mut response = (reply.status, reply.body.replace("{host}", &host)).into_response();
    if let Some(location) = reply.location {
        response.headers_mut().insert(
            header::LOCATION,
            location.replace("{host}", &host).parse().unwrap(),
        );
    }
    response
}

/// The accounts endpoint answers 401 unless basic auth carries exactly
/// the access URL's decoded user and password.
fn bridge(script: Arc<Script>) -> Router {
    let expected = format!("Basic {}", STANDARD.encode(format!("{USER}:{PASSWORD}")));
    Router::new()
        .route(
            "/claim/{id}",
            post(
                |State(script): State<Arc<Script>>, headers: HeaderMap| async move {
                    answer(&script.claims, &headers)
                },
            ),
        )
        .route(
            "/simplefin/accounts",
            get(
                move |State(script): State<Arc<Script>>, headers: HeaderMap| async move {
                    let authorized = headers
                        .get(header::AUTHORIZATION)
                        .is_some_and(|value| value.as_bytes() == expected.as_bytes());
                    if authorized {
                        answer(&script.accounts, &headers)
                    } else {
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                },
            ),
        )
        .route(
            "/target",
            get(|| async { "never requested" }).post(|| async { "never requested" }),
        )
        .with_state(script)
}

struct World {
    server: TestServer,
    mock: MockUpstream,
    script: Arc<Script>,
    alice: Actor,
    writer: SecretToken,
}

async fn world() -> World {
    world_with_env(&[]).await
}

/// Admin "alice" with a write token, on a server whose `SimpleFin`
/// provider talks to a mock bridge only.
async fn world_with_env(vars: &[(&str, &str)]) -> World {
    let script = Arc::new(Script::default());
    let mock = MockUpstream::start(bridge(Arc::clone(&script))).await;
    let server = TestServer::with_env(vars).await.with_simplefin(&mock);
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, writer) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("planner").unwrap(),
            Scope::Write,
            None,
        )
        .await
        .unwrap();
    World {
        server,
        mock,
        script,
        alice,
        writer,
    }
}

/// A setup token: the base64 of a claim URL.
fn setup_token(claim: &str) -> String {
    STANDARD.encode(claim)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

impl World {
    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.mock.base.port().unwrap())
    }

    fn claim_url(&self) -> String {
        format!("http://{}/claim/x", self.host())
    }

    fn access_url(&self) -> String {
        format!("http://{USER}:{PASSWORD_ENCODED}@{}/simplefin", self.host())
    }

    /// Sends `body` to `uri` with the write token; returns the status, the
    /// raw body and the body as JSON.
    async fn call(&self, method: Method, uri: &str, body: &Value) -> (StatusCode, Vec<u8>, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.writer.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, bytes.to_vec(), json)
    }

    async fn connect(&self, token: &str) -> (StatusCode, Vec<u8>, Value) {
        self.call(
            Method::POST,
            "/api/v1/connections/simplefin",
            &json!({ "label": "Family bank", "setup_token": token }),
        )
        .await
    }

    /// A connection whose credentials are the mock's access URL.
    async fn seeded(&self) -> ConnectionId {
        self.server
            .seed_connection(
                ProviderKind::SimpleFin,
                "Family bank",
                self.alice.member,
                &self.access_url(),
            )
            .await
    }

    /// Runs one sync of `connection`, queued directly so no limit applies.
    async fn sync(&self, connection: ConnectionId) -> SyncRunRecord {
        let mut tx = self.server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.alice.member, None)),
                queued_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.server.sync.run(run.id).await.unwrap();
        self.server.store.get_sync_run(run.id).await.unwrap()
    }

    async fn provider_account(&self, provider_account_id: &str) -> ProviderAccountRecord {
        self.server
            .store
            .list_provider_accounts(None)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.provider_account_id == provider_account_id)
            .unwrap()
    }

    /// Creates an account for the provider account and binds it.
    async fn bind(&self, provider_account_id: &str, kind: AccountKind) -> AccountId {
        let held = self.provider_account(provider_account_id).await;
        self.server
            .sync
            .create_account_and_bind(
                &self.alice,
                held.id,
                NewLinkedAccountInput {
                    name: None,
                    kind: Some(kind),
                    liquidity: None,
                    include_in_net_worth: true,
                    include_in_forecast: true,
                },
            )
            .await
            .unwrap()
            .account
            .id
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

    /// Every audit entry, as text, for leak searches.
    async fn audit_text(&self) -> String {
        let events = self
            .server
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
            .items;
        format!("{events:?}")
    }

    async fn connection_count(&self) -> usize {
        self.server.store.list_connections().await.unwrap().len()
    }

    fn opens_to(&self, id: ConnectionId, sealed: Vec<u8>, expected: &str) -> bool {
        self.server
            .vault
            .open(id, &SealedCredentials::from(sealed))
            .is_ok_and(|opened| opened.expose_secret() == expected)
    }
}

/// One account, "Checking" in USD, with `transactions`.
fn account_set(transactions: &Value) -> String {
    json!({
        "errors": [],
        "accounts": [{
            "org": { "domain": "example.com", "name": "Example Bank",
                     "sfin-url": "https://bridge.example/simplefin" },
            "id": "ACT-0001",
            "name": "Checking",
            "currency": "USD",
            "balance": "-1234567.89",
            "balance-date": 1_799_992_800,
            "transactions": transactions,
        }],
    })
    .to_string()
}

#[tokio::test]
async fn claim_stores_sealed_access_url() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin\n"
    )));

    let (status, raw, created) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["provider"], json!("simplefin"));
    assert_eq!(created["status"], json!("active"));
    let id: ConnectionId = created["id"].as_str().unwrap().parse().unwrap();
    let requests = world.mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(requests[0].path, "/claim/x");
    assert!(!requests[0].authorization);

    let sealed = world
        .server
        .store
        .get_connection(id)
        .await
        .unwrap()
        .credentials
        .unwrap();
    assert!(!contains(&sealed, USER));
    assert!(!contains(&sealed, PASSWORD_MARKER));
    assert!(world.opens_to(id, sealed, &world.access_url()));
    assert!(!contains(&raw, PASSWORD_MARKER));
    assert!(!world.audit_text().await.contains(PASSWORD_MARKER));
    let log = captured_log_text();
    assert!(!log.contains(PASSWORD_MARKER));
    assert!(log.contains(&id.to_string()));
}

#[tokio::test]
async fn setup_token_for_unlisted_host_refused() {
    let world = world().await;
    for claim in [
        "https://internal.example/claim/x",
        "https://10.0.0.1/claim/x",
        "https://bridge.simplefin.org.evil.test/claim/x",
        "https://bridge.simplefin.org/claim/x",
        "http://127.0.0.1:1/claim/x",
    ] {
        let (status, _, problem) = world.connect(&setup_token(claim)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
    }
    assert_eq!(world.mock.requests().len(), 0);
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn access_url_for_unlisted_host_refused() {
    let world = world().await;
    for access in [
        format!("https://{USER}:{PASSWORD_ENCODED}@internal.example/simplefin"),
        format!("http://{USER}:{PASSWORD_ENCODED}@127.0.0.1:1/simplefin"),
        // No password, no user information, or a query.
        format!("http://{USER}@{{host}}/simplefin"),
        "http://{host}/simplefin".to_owned(),
        format!("http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin?x=1"),
        "not a url".to_owned(),
    ] {
        world.script.claim(Reply::ok(&access));
        let (status, raw, problem) = world.connect(&setup_token(&world.claim_url())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
        assert!(!contains(&raw, PASSWORD_MARKER));
    }
    // An oversized answer is refused too.
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin/{}",
        "a".repeat(2100)
    )));
    let (status, _, _) = world.connect(&setup_token(&world.claim_url())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(world.connection_count().await, 0);
    assert!(
        world
            .mock
            .requests()
            .iter()
            .all(|request| request.path == "/claim/x")
    );
}

#[tokio::test]
async fn claim_redirect_not_followed() {
    let world = world().await;
    world.script.claim(Reply {
        status: StatusCode::FOUND,
        body: String::new(),
        location: Some("http://{host}/target".to_owned()),
    });

    let (status, _, problem) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("setup_token_invalid"));
    let paths: Vec<String> = world
        .mock
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(paths, vec!["/claim/x".to_owned()]);
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn claim_errors_map_to_their_problems() {
    let world = world().await;
    for (reply, status, code) in [
        (
            Reply::status(StatusCode::FORBIDDEN, "token already claimed"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "setup_token_invalid",
        ),
        (
            Reply::status(StatusCode::NOT_FOUND, ""),
            StatusCode::UNPROCESSABLE_ENTITY,
            "setup_token_invalid",
        ),
        (
            Reply::status(StatusCode::SERVICE_UNAVAILABLE, "maintenance"),
            StatusCode::BAD_GATEWAY,
            "provider_error",
        ),
    ] {
        world.script.claim(reply);
        let (got, _, problem) = world.connect(&setup_token(&world.claim_url())).await;
        assert_eq!(got, status);
        assert_eq!(problem["code"], json!(code));
    }
    assert_eq!(world.connection_count().await, 0);
}

#[tokio::test]
async fn setup_token_never_echoed() {
    let world = world().await;
    let oversized = format!("{BOGUS_TOKEN}{}", "A".repeat(2100));
    let not_a_url = STANDARD.encode(format!("{BOGUS_TOKEN} is no url"));
    for token in [BOGUS_TOKEN, oversized.as_str(), not_a_url.as_str(), ""] {
        let (status, raw, problem) = world.connect(token).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["code"], json!("setup_token_invalid"));
        assert!(!contains(&raw, "SECRETTOKEN"));
    }
    assert!(!world.audit_text().await.contains("SECRETTOKEN"));
    assert!(!captured_log_text().contains("SECRETTOKEN"));
    assert_eq!(world.mock.requests().len(), 0);
}

/// Error answers that echo the setup token, the claim URL, the access URL
/// or its password reach no response, audit entry, run detail,
/// connection error or log line.
#[tokio::test]
async fn upstream_errors_never_echo_credentials() {
    let world = world().await;
    let claim_marker = "/claim/x";
    let token = setup_token(&world.claim_url());
    let echo = format!(
        "token {token} claim {} access {} password {PASSWORD}",
        world.claim_url(),
        world.access_url()
    );
    for status in [StatusCode::FORBIDDEN, StatusCode::INTERNAL_SERVER_ERROR] {
        world.script.claim(Reply::status(status, &echo));
        let (_, raw, _) = world.connect(&token).await;
        assert!(!contains(&raw, &token));
        assert!(!contains(&raw, claim_marker));
        assert!(!contains(&raw, PASSWORD_MARKER));
    }
    // A claim answered with garbage that holds the password.
    world
        .script
        .claim(Reply::ok(&format!("{PASSWORD} {}", world.access_url())));
    let (_, raw, _) = world.connect(&token).await;
    assert!(!contains(&raw, PASSWORD_MARKER));

    let connection = world.seeded().await;
    let mut details = Vec::new();
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_REQUEST,
        StatusCode::PAYMENT_REQUIRED,
        StatusCode::FORBIDDEN,
    ] {
        world.script.accounts(Reply::status(status, &echo));
        let run = world.sync(connection).await;
        assert_eq!(run.status, SyncRunStatus::Failed);
        details.push(run.detail.unwrap_or_default());
    }
    // A 200 answer that cannot be read, and one whose errors echo the
    // password.
    world.script.accounts(Reply::ok(&echo));
    details.push(world.sync(connection).await.detail.unwrap_or_default());
    world.script.accounts(Reply::ok(
        &json!({ "errors": [echo], "accounts": [] }).to_string(),
    ));
    details.push(world.sync(connection).await.detail.unwrap_or_default());

    let record = world.server.store.get_connection(connection).await.unwrap();
    let mut stored = details.join("\n");
    stored.push_str(&record.last_error.unwrap_or_default());
    let audit = world.audit_text().await;
    let log = captured_log_text();
    for text in [&stored, &audit, &log] {
        assert!(!text.contains(&token));
        assert!(!text.contains(PASSWORD_MARKER));
        assert!(!text.contains(USER));
    }
    assert!(!stored.contains(claim_marker));
}

#[tokio::test]
async fn fetch_sends_window_and_basic_auth() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(ACCOUNTS_FIRST));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let requests = world.mock.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path, "/simplefin/accounts");
    // The first sync asks for 90 days before today (2027-01-15, UTC).
    let start = date(2026, 10, 17)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_second();
    assert_eq!(
        request.query.as_deref(),
        Some(format!("start-date={start}&pending=1").as_str())
    );
    assert!(request.authorization);
    assert!(!request.path.contains(USER));
    assert!(
        !request
            .query
            .as_deref()
            .unwrap_or_default()
            .contains(PASSWORD_MARKER)
    );
}

async fn category(world: &World) -> CategoryId {
    let group = world
        .server
        .ledger
        .create_category_group(
            &world.alice,
            CategoryGroupInput {
                name: "Everyday".into(),
                position: None,
            },
            None,
        )
        .await
        .unwrap();
    world
        .server
        .ledger
        .create_category(
            &world.alice,
            CategoryInput {
                group: group.id,
                name: "Coffee".into(),
                kind: CategoryKind::Expense,
                position: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn sync_from_fixtures_then_posted_keeps_state() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(ACCOUNTS_FIRST));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );
    let checking = world.bind("ACT-0001", AccountKind::Checking).await;
    let card = world.bind("ACT-0002", AccountKind::CreditCard).await;
    assert_eq!(world.transactions(card).await.len(), 1);
    let first = world.transactions(checking).await;
    assert_eq!(first.len(), 3);
    let pending = first.iter().find(|txn| txn.pending).unwrap();
    assert_eq!(pending.amount_minor, -1825);
    // Pending: dated by `transacted_at` (2027-01-14T20:45Z).
    assert_eq!(pending.posted_on, date(2027, 1, 14));
    let coffee = category(&world).await;
    world
        .server
        .ledger
        .update_transaction(
            &world.alice,
            pending.id,
            TransactionPatch {
                splits: Some(vec![SplitInput {
                    category: coffee,
                    minor_units: -1825,
                }]),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    world.script.accounts(Reply::ok(ACCOUNTS_POSTED));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );

    let after = world.transactions(checking).await;
    assert_eq!(after.len(), 3);
    let posted = after.iter().find(|txn| txn.id == pending.id).unwrap();
    assert!(!posted.pending);
    assert_eq!(posted.posted_on, date(2027, 1, 15));
    assert_eq!(posted.splits.len(), 1);
    assert_eq!(posted.splits[0].category, coffee);
    assert!(after.iter().all(|txn| !txn.pending));
}

#[tokio::test]
async fn posted_date_uses_household_timezone() {
    let world = world_with_env(&[("COFFER_TIMEZONE", "America/Los_Angeles")]).await;
    let connection = world.seeded().await;
    // 2026-10-31T06:30Z is still 2026-10-30 in Los Angeles.
    world.script.accounts(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_793_428_200, "amount": "-5.00",
        "description": "PARKING METER",
    }]))));

    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].posted_on, date(2026, 10, 30));
}

#[tokio::test]
async fn errors_fixture_reports_connection_and_account_errors() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(ACCOUNTS_ERRORS));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(
        record.last_error.as_deref(),
        Some("Example Bank: the connection needs attention")
    );
    let points = world.provider_account("ACT-0003").await;
    assert!(points.error.is_some());
    assert_eq!(points.currency_raw, "https://example.com/points");
    let malformed = world.provider_account("ACT-0004").await;
    assert_eq!(malformed.name, "Savings");
    assert!(malformed.error.is_some());
}

#[tokio::test]
async fn reauth_on_403() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .accounts(Reply::status(StatusCode::FORBIDDEN, "access revoked"));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::ReauthRequired);
}

#[tokio::test]
async fn rate_limited_on_429() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .accounts(Reply::status(StatusCode::TOO_MANY_REQUESTS, "slow down"));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("the provider is rate limiting requests")
    );
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::Error);
}

#[tokio::test]
async fn subscription_inactive_on_402() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .accounts(Reply::status(StatusCode::PAYMENT_REQUIRED, ""));

    let run = world.sync(connection).await;

    assert_eq!(
        run.detail.as_deref(),
        Some("SimpleFin subscription inactive")
    );
}

#[tokio::test]
async fn amounts_parsed_exactly() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": "-1234567.89",
        "description": "LARGE TRANSFER", "extra": { "category": "transfer" },
    }]))));

    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].amount_minor, -123_456_789);
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.last_balance.unwrap().amount_raw, "-1234567.89");
}

/// A number where the protocol has a decimal string makes that account an
/// error; it is never read as a float.
#[tokio::test]
async fn numeric_amount_is_an_account_error() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": -12.5,
        "description": "FLOAT AMOUNT",
    }]))));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let held = world.provider_account("ACT-0001").await;
    assert!(held.error.is_some());
    assert_eq!(
        world
            .server
            .store
            .list_provider_accounts(None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn reconnect_with_new_setup_token() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.accounts(Reply::ok(ACCOUNTS_FIRST));
    world.sync(connection).await;
    let checking = world.bind("ACT-0001", AccountKind::Checking).await;
    world
        .script
        .accounts(Reply::status(StatusCode::FORBIDDEN, ""));
    world.sync(connection).await;
    assert_eq!(
        world
            .server
            .store
            .get_connection(connection)
            .await
            .unwrap()
            .status,
        ConnectionStatus::ReauthRequired
    );

    let renewed = format!("http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin");
    world.script.claim(Reply::ok(&renewed));
    let (status, raw, view) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{connection}/simplefin/reconnect"),
            &json!({ "setup_token": setup_token(&world.claim_url()) }),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["status"], json!("active"));
    assert!(!contains(&raw, PASSWORD_MARKER));
    let bound = world.provider_account("ACT-0001").await;
    assert_eq!(bound.account, Some(checking));
    let record = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(record.status, ConnectionStatus::Active);
    assert!(world.opens_to(connection, record.credentials.unwrap(), &world.access_url()));
}

#[tokio::test]
async fn reconnect_with_invalid_token_changes_nothing() {
    let world = world().await;
    let connection = world.seeded().await;
    let before = world.server.store.get_connection(connection).await.unwrap();

    let (status, raw, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{connection}/simplefin/reconnect"),
            &json!({ "setup_token": BOGUS_TOKEN }),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("setup_token_invalid"));
    assert!(!contains(&raw, "SECRETTOKEN"));
    let after = world.server.store.get_connection(connection).await.unwrap();
    assert_eq!(after.credentials, before.credentials);
}

/// A read token cannot connect, and nothing reaches the bridge.
#[tokio::test]
async fn connect_needs_manage_connections() {
    let mut world = world().await;
    let (_, reader) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
        )
        .await
        .unwrap();
    world.writer = reader;

    let (status, _, problem) = world.connect(&setup_token(&world.claim_url())).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("forbidden"));
    assert_eq!(world.mock.requests().len(), 0);
}

/// A retry with the same `Idempotency-Key` replays the first response and
/// claims nothing again.
#[tokio::test]
async fn connect_with_idempotency_key_replays() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin"
    )));
    let body = json!({ "label": "Family bank", "setup_token": setup_token(&world.claim_url()) });
    let send = || async {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/connections/simplefin")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", world.writer.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .header("Idempotency-Key", "connect-1")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router(world.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes)
    };

    let (first_status, first) = send().await;
    let (second_status, second) = send().await;

    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(first, second);
    assert_eq!(world.connection_count().await, 1);
    assert_eq!(world.mock.requests().len(), 1);
}

/// The provider registered by the configuration, connected directly:
/// the label is the caller's, the credentials the access URL.
#[tokio::test]
async fn service_connect_returns_the_access_url() {
    let world = world().await;
    world.script.claim(Reply::ok(&format!(
        "http://{USER}:{PASSWORD_ENCODED}@{{host}}/simplefin"
    )));

    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::SimpleFin,
            ConnectionLabel::parse("Joint").unwrap(),
            ConnectInput::SimpleFinSetupToken(SecretString::from(setup_token(&world.claim_url()))),
            None,
        )
        .await
        .unwrap();

    assert_eq!(view.label, "Joint");
    let sealed = world
        .server
        .store
        .get_connection(view.id)
        .await
        .unwrap()
        .credentials
        .unwrap();
    assert!(world.opens_to(view.id, sealed, &world.access_url()));
}
