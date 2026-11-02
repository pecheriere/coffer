//! Plaid behind the provider interface: real sync runs against a loopback
//! mock of Plaid's API, from `/transactions/sync` pages to stored
//! transactions, cursors, connection states and removals.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod credentials;
mod failures;
mod providers;
mod reauthorized;
mod removal;
mod transactions;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, SyncRunId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::CategoryKind;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::{CallGate, FakeProvider, MockUpstream, TestServer};
use coffer_store::{
    AuditEvent, AuditFilter, ConnectionRecord, NewSyncRun, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, SyncRunRecord, TransactionFilter, TransactionRecord,
};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tower::ServiceExt as _;

const ACCOUNTS: &str = include_str!("../fixtures/plaid/accounts_get.json");
const PAGE_1: &str = include_str!("../fixtures/plaid/transactions_sync_page1.json");
const PAGE_2: &str = include_str!("../fixtures/plaid/transactions_sync_page2.json");
const POSTED: &str = include_str!("../fixtures/plaid/transactions_sync_posted.json");
const LOGIN_REQUIRED: &str = include_str!("../fixtures/plaid/error_item_login_required.json");
const MUTATION: &str = include_str!("../fixtures/plaid/error_mutation_during_pagination.json");
const ITEM_REMOVE: &str = include_str!("../fixtures/plaid/item_remove.json");

/// The access token of the seeded connection; recognisable in a text search.
const ACCESS_MARKER: &str = "ACCESSTOKEN-MARKER-71";
const CREDENTIALS: &str = r#"{"access_token":"access-ACCESSTOKEN-MARKER-71","item_id":"item-1"}"#;
const SYNC: &str = "/transactions/sync";
const GET_ACCOUNTS: &str = "/accounts/get";
const REMOVE: &str = "/item/remove";
const EXCHANGE: &str = "/item/public_token/exchange";

/// A status and a body to answer with.
type Reply = (StatusCode, String);

#[derive(Clone, Default)]
struct Script {
    /// Answers by path, each given once.
    replies: Arc<Mutex<HashMap<String, VecDeque<Reply>>>>,
    /// The answer by path once there is none of the above.
    fallback: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    /// Holds the next request once it has been seen.
    gate: Arc<Mutex<Option<CallGate>>>,
}

impl Script {
    /// Makes the next request wait, whatever its path, until the returned
    /// gate is released; its answer is chosen after that.
    fn hold_next(&self) -> CallGate {
        let gate = CallGate::default();
        *self.gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    fn reply(&self, path: &str, status: StatusCode, body: impl Into<String>) {
        self.replies
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .push_back((status, body.into()));
    }

    fn page(&self, body: impl Into<String>) {
        self.reply(SYNC, StatusCode::OK, body);
    }

    fn always(&self, path: &str, status: StatusCode, body: impl Into<String>) {
        self.fallback
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.into()));
    }

    fn seen(&self, path: &str) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| seen == path)
            .map(|(_, body)| body.clone())
            .collect()
    }

    /// The cursor each `/transactions/sync` request carried.
    fn cursors(&self) -> Vec<Option<String>> {
        self.seen(SYNC)
            .iter()
            .map(|body| body.get("cursor").map(|c| c.as_str().unwrap().to_owned()))
            .collect()
    }
}

async fn answer(State(script): State<Script>, uri: Uri, body: String) -> Response {
    let path = uri.path().to_owned();
    script
        .seen
        .lock()
        .unwrap()
        .push((path.clone(), serde_json::from_str(&body).unwrap()));
    let gate = script.gate.lock().unwrap().take();
    if let Some(gate) = gate {
        gate.hold().await;
    }
    let scripted = script
        .replies
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front);
    let (status, reply) = scripted
        .or_else(|| script.fallback.lock().unwrap().get(&path).cloned())
        .unwrap_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "no scripted reply".to_owned(),
        ));
    (status, [(header::CONTENT_TYPE, "application/json")], reply).into_response()
}

struct World {
    server: TestServer,
    script: Script,
    _mock: MockUpstream,
    fake: Arc<FakeProvider>,
    alice: Actor,
    connection: ConnectionId,
}

/// Admin "alice" with a seeded Plaid connection, whose accounts default to
/// the fixture's, and a scripted `SimpleFin`-kind provider beside it.
async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
    let script = Script::default();
    script.always(GET_ACCOUNTS, StatusCode::OK, ACCOUNTS);
    let mock = MockUpstream::start(Router::new().fallback(answer).with_state(script.clone())).await;
    let server = server.with_plaid(&mock);
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let connection = server
        .seed_connection(ProviderKind::Plaid, "Plaid bank", member, CREDENTIALS)
        .await;
    World {
        server,
        script,
        _mock: mock,
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
    async fn sync(&self) -> SyncRunRecord {
        self.sync_connection(self.connection).await
    }

    async fn sync_connection(&self, connection: ConnectionId) -> SyncRunRecord {
        let id = self.queue(connection).await;
        self.server.sync.run(id).await.unwrap();
        self.server.store.get_sync_run(id).await.unwrap()
    }

    async fn queue(&self, connection: ConnectionId) -> SyncRunId {
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
        run.id
    }

    async fn connection(&self) -> ConnectionRecord {
        self.server
            .store
            .get_connection(self.connection)
            .await
            .unwrap()
    }

    async fn account(&self) -> AccountId {
        self.server
            .app_state()
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: "Bound".into(),
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

    /// Binds a provider account of `connection` to `account`.
    async fn bind_to(&self, connection: ConnectionId, id: &str, account: AccountId) {
        let mut tx = self.server.store.begin().await.unwrap();
        let provider_account = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection,
                provider_account_id: id.into(),
                name: id.into(),
                institution: None,
                currency_raw: "USD".into(),
                kind_hint: None,
                balance: None,
                seen_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.bind_provider_account(provider_account.id, account)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    /// A new account bound to the provider account `id` of the connection.
    async fn bound(&self, id: &str) -> AccountId {
        let account = self.account().await;
        self.bind_to(self.connection, id, account).await;
        account
    }

    async fn provider_account(&self, id: &str) -> ProviderAccountRecord {
        self.server
            .store
            .list_provider_accounts(Some(self.connection))
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.provider_account_id == id)
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
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap()
            .items
    }

    /// How many imported updates the sync has audited so far.
    async fn imported_updates(&self) -> usize {
        self.audit()
            .await
            .iter()
            .filter(|event| {
                event.action == "transaction.imported_update"
                    && matches!(event.actor, AuditActor::System { .. })
            })
            .count()
    }

    async fn category(&self) -> CategoryId {
        let ledger = self.server.app_state().ledger;
        let group = ledger
            .create_category_group(
                &self.alice,
                CategoryGroupInput {
                    name: "Food group".into(),
                    position: None,
                },
                None,
            )
            .await
            .unwrap();
        ledger
            .create_category(
                &self.alice,
                CategoryInput {
                    group: group.id,
                    name: "Food".into(),
                    kind: CategoryKind::Expense,
                    position: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    /// `DELETE` of the connection through the router, with a write token.
    async fn delete_connection(&self) -> (StatusCode, Value) {
        let (_, token) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                Scope::Write,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        let request = Request::builder()
            .method(Method::DELETE)
            .uri(format!("/api/v1/connections/{}", self.connection))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
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
}

/// One record of `added` or `modified`.
fn record(id: &str, account: &str, amount: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{"transaction_id":"{id}","account_id":"{account}","amount":{amount},
            "date":"2027-01-10","authorized_date":null,"name":"SHOP {id}",
            "merchant_name":null,"pending":false,"pending_transaction_id":null,
            "iso_currency_code":"USD","unofficial_currency_code":null}}"#
    ))
    .unwrap()
}

fn page(
    added: &[Value],
    modified: &[Value],
    removed: &[Value],
    cursor: &str,
    more: bool,
) -> String {
    json!({
        "added": added,
        "modified": modified,
        "removed": removed,
        "next_cursor": cursor,
        "has_more": more,
    })
    .to_string()
}

fn accounts_answer(accounts: &[Value]) -> String {
    json!({ "accounts": accounts }).to_string()
}

fn account_json(id: &str, kind: &str, current: &Value) -> Value {
    json!({
        "account_id": id,
        "name": format!("Account {id}"),
        "type": kind,
        "subtype": null,
        "balances": { "current": current, "iso_currency_code": "USD" },
    })
}
