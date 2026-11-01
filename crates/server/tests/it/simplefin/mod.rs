//! The `SimpleFin` provider against a loopback mock of the bridge: the
//! claim and its SSRF checks, the accounts request, parsing of recorded
//! generic fixtures, error mapping, and credentials that never reach the
//! database in clear, an API response, the audit, a run's detail or a log.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod claim;
mod connect;
mod fetch;
mod sync;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::connections::NewLinkedAccountInput;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::{MockUpstream, TestServer};
use coffer_server::vault::SealedCredentials;
use coffer_store::{
    AuditFilter, NewSyncRun, PageRequest, ProviderAccountRecord, SyncRunRecord, TransactionFilter,
    TransactionRecord,
};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tower::ServiceExt as _;

const ACCOUNTS_FIRST: &str = include_str!("../fixtures/simplefin/accounts_first.json");
const ACCOUNTS_POSTED: &str = include_str!("../fixtures/simplefin/accounts_posted.json");
const ACCOUNTS_ERRORS: &str = include_str!("../fixtures/simplefin/accounts_errors.json");

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
    bridge_replies: Mutex<VecDeque<Reply>>,
}

impl Script {
    fn claim(&self, reply: Reply) {
        self.claims.lock().unwrap().push_back(reply);
    }

    fn bridge_reply(&self, reply: Reply) {
        self.bridge_replies.lock().unwrap().push_back(reply);
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
                        answer(&script.bridge_replies, &headers)
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
            Vec::new(),
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
