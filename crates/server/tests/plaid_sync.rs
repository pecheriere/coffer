//! Plaid behind the provider interface: real sync runs against a loopback
//! mock of Plaid's API, from `/transactions/sync` pages to stored
//! transactions, cursors, connection states and removals.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, SyncRunId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::config::Config;
use coffer_server::egress::Egress;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::sync::SyncError;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{
    ConnectInput, FetchMode, FetchResult, ProviderAccountData, ProviderError, ProviderKind,
    ProviderRegistry, ProviderTransaction,
};
use coffer_server::sync::scheduler::{ScheduleEntry, due};
use coffer_server::testkit::{
    FakeProvider, INJECTED_FAILURE_RECORD_ID, MockUpstream, TestServer, captured_log_text,
    log_marker,
};
use coffer_store::{
    AuditEvent, AuditFilter, ConnectionRecord, ConnectionStatus, NewSyncRun, PageRequest,
    ProviderAccountRecord, ProviderAccountUpsert, SyncRunRecord, TransactionFilter,
    TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::SignedDuration;
use jiff::civil::date;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ACCOUNTS: &str = include_str!("fixtures/plaid/accounts_get.json");
const PAGE_1: &str = include_str!("fixtures/plaid/transactions_sync_page1.json");
const PAGE_2: &str = include_str!("fixtures/plaid/transactions_sync_page2.json");
const POSTED: &str = include_str!("fixtures/plaid/transactions_sync_posted.json");
const LOGIN_REQUIRED: &str = include_str!("fixtures/plaid/error_item_login_required.json");
const MUTATION: &str = include_str!("fixtures/plaid/error_mutation_during_pagination.json");
const ITEM_REMOVE: &str = include_str!("fixtures/plaid/item_remove.json");

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
}

impl Script {
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

#[tokio::test]
async fn first_sync_pages_until_done_and_saves_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(PAGE_1);
    world.script.page(PAGE_2);

    let run = world.sync().await;

    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 1);
    assert_eq!(
        world.script.cursors(),
        [None, Some("cursor-page-1".to_owned())],
        "the second request carries the first page's cursor"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-page-2")
    );
    assert_eq!(world.connection().await.status, ConnectionStatus::Active);
    // The crypto account's currency is refused, which the run reports.
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(checking).await.len(), 2);
}

#[tokio::test]
async fn next_sync_starts_from_saved_cursor() {
    let world = world().await;
    world.script.page(POSTED);
    world.sync().await;
    world
        .script
        .page(page(&[], &[], &[], "cursor-after", false));

    world.sync().await;

    assert_eq!(
        world.script.cursors(),
        [None, Some("cursor-page-3".to_owned())]
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-after")
    );
}

/// Plaid's positive is money out: a `12` from Plaid is -12.00 here, and an
/// income's negative comes out positive. Dates and texts map as documented.
#[tokio::test]
async fn plaid_amount_sign_inverted() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(PAGE_1);
    world.script.page(page(
        &[record("income", "acct-checking", "-1500.25")],
        &[],
        &[],
        "c",
        false,
    ));

    world.sync().await;

    let transactions = world.transactions(checking).await;
    let spent = transactions
        .iter()
        .find(|t| t.raw_description.as_str() == "CORNER COFFEE 0001")
        .unwrap();
    assert_eq!(spent.amount_minor, -1200);
    assert_eq!(spent.currency.as_str(), "USD");
    assert_eq!(spent.posted_on, date(2027, 1, 10));
    assert_eq!(spent.transacted_on, Some(date(2027, 1, 9)));
    assert_eq!(
        spent.imported_payee.as_ref().unwrap().as_str(),
        "Corner Coffee"
    );
    assert!(!spent.pending);
    assert_eq!(
        spent.sources[0].amount_raw, "12",
        "the provider's exact text, in its own sign"
    );
    let income = transactions
        .iter()
        .find(|t| t.raw_description.as_str() == "SHOP income")
        .unwrap();
    assert_eq!(income.amount_minor, 150_025);
}

#[tokio::test]
async fn liability_balances_are_negative() {
    let world = world().await;
    let card = world.bound("acct-card").await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(&[], &[], &[], "c", false));

    world.sync().await;

    let today = world
        .server
        .clock
        .now()
        .to_zoned(world.server.config.timezone.clone())
        .date();
    let owed = world
        .server
        .store
        .latest_balance(card)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owed.balance_minor, -41_000, "the amount owed is negative");
    assert_eq!(owed.on, today);
    let held = world
        .server
        .store
        .latest_balance(checking)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        held.balance_minor, 11_050,
        "a deposit balance keeps its sign"
    );
    assert_eq!(
        world.provider_account("acct-card").await.kind_hint,
        Some(AccountKind::CreditCard)
    );
    assert_eq!(
        world.provider_account("acct-checking").await.kind_hint,
        Some(AccountKind::Checking)
    );
}

/// The posted form, under a new id and with another amount, joins the pending
/// transaction through `pending_transaction_id`, and what the member did to
/// it stays. Plaid names the pending record as removed in the same answer.
#[tokio::test]
async fn posted_successor_attaches_via_pending_transaction_id() {
    let world = world().await;
    let card = world.bound("acct-card").await;
    world.script.page(PAGE_1);
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    let pending = world.transactions(card).await.remove(0);
    assert!(pending.pending);
    assert_eq!(pending.amount_minor, -450);
    let food = world.category().await;
    world
        .server
        .app_state()
        .ledger
        .update_transaction(
            &world.alice,
            pending.id,
            TransactionPatch {
                splits: Some(vec![SplitInput {
                    category: food,
                    minor_units: -450,
                }]),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    world.script.page(POSTED);
    world.sync().await;

    let after = world.transactions(card).await;
    assert_eq!(after.len(), 1, "no second transaction");
    let posted = &after[0];
    assert_eq!(posted.id, pending.id);
    assert!(!posted.pending);
    assert_eq!(posted.amount_minor, -475);
    assert_eq!(posted.posted_on, date(2027, 1, 13));
    assert_eq!(posted.splits.len(), 1, "the category is kept");
    assert_eq!(posted.splits[0].category, food);
    assert_eq!(posted.splits[0].minor_units, -475, "the split follows");
    let sources: Vec<_> = posted
        .sources
        .iter()
        .map(|source| source.provider_txn_id.as_str())
        .collect();
    assert_eq!(sources, ["txn-posted"], "the pending record is gone");
}

#[tokio::test]
async fn removed_ids_remove_sources() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("stays", "acct-checking", "1"),
            record("goes", "acct-checking", "2"),
        ],
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;
    assert_eq!(world.transactions(checking).await.len(), 2);

    world.script.page(page(
        &[],
        &[],
        &[json!({ "transaction_id": "goes", "account_id": "acct-checking" })],
        "c2",
        false,
    ));
    world.sync().await;

    let left = world.transactions(checking).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].sources[0].provider_txn_id, "stays");
}

/// A removal counts for the account it names: naming another account leaves
/// the record where it is.
#[tokio::test]
async fn removal_is_attributed_to_its_account() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    let card = world.bound("acct-card").await;
    world.script.page(page(
        &[
            record("in-checking", "acct-checking", "1"),
            record("in-card", "acct-card", "2"),
        ],
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;

    world.script.page(page(
        &[],
        &[],
        &[
            json!({ "transaction_id": "in-checking", "account_id": "acct-card" }),
            json!({ "transaction_id": "in-card", "account_id": "acct-card" }),
        ],
        "c2",
        false,
    ));
    world.sync().await;

    assert_eq!(world.transactions(checking).await.len(), 1);
    assert_eq!(world.transactions(card).await.len(), 0);
}

/// Pages are netted: a record's last state in the fetch counts, so one added
/// and removed within a fetch is never stored, and a modification wins.
#[tokio::test]
async fn pages_are_netted_by_record() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("short-lived", "acct-checking", "1"),
            record("changed", "acct-checking", "2"),
        ],
        &[],
        &[],
        "c1",
        true,
    ));
    world.script.page(page(
        &[],
        &[record("changed", "acct-checking", "3")],
        &[json!({ "transaction_id": "short-lived", "account_id": "acct-checking" })],
        "c2",
        false,
    ));

    world.sync().await;

    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "changed");
    assert_eq!(stored[0].amount_minor, -300);
    assert_eq!(world.imported_updates().await, 0, "nothing to rewrite");
}

#[tokio::test]
async fn mutation_during_pagination_restarts() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[record("abandoned", "acct-checking", "1")],
        &[],
        &[],
        "stale",
        true,
    ));
    world.script.reply(SYNC, StatusCode::BAD_REQUEST, MUTATION);
    world.script.page(page(
        &[record("kept", "acct-checking", "2")],
        &[],
        &[],
        "fresh",
        true,
    ));
    world.script.page(page(&[], &[], &[], "done", false));

    let run = world.sync().await;

    assert_eq!(
        world.script.cursors(),
        [
            None,
            Some("stale".to_owned()),
            None,
            Some("fresh".to_owned())
        ],
        "the restart uses the cursor the fetch started with"
    );
    let stored = world.transactions(checking).await;
    assert_eq!(
        stored.len(),
        1,
        "nothing of the abandoned pass is committed"
    );
    assert_eq!(stored[0].sources[0].provider_txn_id, "kept");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("done")
    );
    assert_ne!(run.status, SyncRunStatus::Failed);
}

#[tokio::test]
async fn data_that_keeps_changing_gives_up_after_three_passes() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(&[], &[], &[], "saved", false));
    world.sync().await;
    world.script.always(SYNC, StatusCode::BAD_REQUEST, MUTATION);
    let before = world.script.cursors().len();

    let run = world.sync().await;

    assert_eq!(world.script.cursors().len() - before, 3, "three passes");
    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("Plaid data changed during sync; try again")
    );
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("saved"));
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(world.transactions(checking).await.len(), 0);
}

#[tokio::test]
async fn item_login_required_marks_reauth() {
    let world = world().await;
    world
        .script
        .reply(GET_ACCOUNTS, StatusCode::BAD_REQUEST, LOGIN_REQUIRED);

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world.connection().await;
    assert_eq!(connection.status, ConnectionStatus::ReauthRequired);
    let entry = ScheduleEntry {
        id: connection.id,
        status: connection.status,
        last_attempt_at: None,
        effective_interval: SignedDuration::from_hours(1),
    };
    assert_eq!(
        due(world.server.clock.now(), &[entry]),
        [],
        "scheduled syncs stop spending requests on it"
    );

    // The same from a page of transactions.
    let world2 = self::world().await;
    world2
        .script
        .reply(SYNC, StatusCode::BAD_REQUEST, LOGIN_REQUIRED);
    world2.sync().await;
    assert_eq!(
        world2.connection().await.status,
        ConnectionStatus::ReauthRequired
    );
}

#[tokio::test]
async fn disconnect_removes_item_or_fails_cleanly() {
    let world = world().await;
    let before = world.connection().await;
    world
        .script
        .reply(REMOVE, StatusCode::INTERNAL_SERVER_ERROR, "");

    let (status, problem) = world.delete_connection().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(problem["code"], json!("provider_error"));
    let kept = world.connection().await;
    assert_eq!(kept.status, before.status);
    assert!(
        kept.credentials == before.credentials,
        "the credentials are kept"
    );

    world.script.reply(REMOVE, StatusCode::OK, ITEM_REMOVE);
    let (status, _) = world.delete_connection().await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let removed = world.connection().await;
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert!(removed.credentials.is_none(), "the credentials are erased");
    let requests = world.script.seen(REMOVE);
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["access_token"] == format!("access-{ACCESS_MARKER}").as_str(),
        "the removal names the Item's access token"
    );
}

/// An Item already gone at Plaid must not keep its connection forever.
#[tokio::test]
async fn disconnect_of_an_item_already_gone_succeeds() {
    let world = world().await;
    world.script.reply(
        REMOVE,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": "ITEM_ERROR", "error_code": "ITEM_NOT_FOUND" }).to_string(),
    );

    let (status, _) = world.delete_connection().await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(world.connection().await.status, ConnectionStatus::Removed);
}

/// Credentials that do not parse cannot be used at Plaid either: they do not
/// stop a removal, and a sync says to reconnect without quoting them.
#[tokio::test]
async fn unusable_credentials_fail_sync_and_do_not_block_removal() {
    let world = world().await;
    let garbled = world
        .server
        .seed_connection(
            ProviderKind::Plaid,
            "Garbled",
            world.alice.member,
            "not-json-ACCESSTOKEN-MARKER-71",
        )
        .await;

    let run = world.sync_connection(garbled).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let detail = run.detail.unwrap();
    assert!(detail.contains("reconnect"), "the run says what to do");
    assert!(!detail.contains(ACCESS_MARKER), "and quotes nothing");
    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 0);

    world
        .server
        .sync
        .remove_connection(&world.alice, garbled)
        .await
        .unwrap();
    let log = captured_log_text();
    let warned = log.lines().any(|line| {
        line.contains("removed without disconnecting") && line.contains(&garbled.to_string())
    });
    assert!(warned, "a removal that gave up nothing at Plaid is logged");
    assert!(!log.contains(ACCESS_MARKER), "and quotes nothing");
}

/// One unreadable account leaves the others syncing. Its records were not
/// stored, so the cursor stays and the next sync fetches them again, without
/// duplicating what the others already hold.
#[tokio::test]
async fn bad_account_is_isolated_and_holds_the_cursor() {
    let world = world().await;
    let good = world.bound("good").await;
    let bad = world.bound("bad").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[
            {"account_id":"good","name":"Good","type":"depository","subtype":"checking",
             "balances":{"current":5,"iso_currency_code":"USD"}},
            {"account_id":"bad","name":"Bad","type":"depository",
             "balances":{"current":1e2,"iso_currency_code":"USD"}}]}"#,
    );
    let records = page(
        &[record("g1", "good", "1"), record("b1", "bad", "2")],
        &[],
        &[],
        "cursor-1",
        false,
    );
    world.script.page(records.clone());

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(good).await.len(), 1);
    assert_eq!(world.transactions(bad).await.len(), 0);
    assert!(
        world
            .provider_account("bad")
            .await
            .error
            .unwrap()
            .contains("could not read"),
        "the bad account carries the error"
    );
    assert_eq!(world.provider_account("good").await.error, None);
    assert_eq!(
        world.connection().await.provider_state,
        None,
        "the cursor does not pass records that were not stored"
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[
            account_json("good", "depository", &json!(5)),
            account_json("bad", "depository", &json!(100)),
        ]),
    );
    world.script.page(records);

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    assert_eq!(world.script.cursors(), [None, None]);
    assert_eq!(world.transactions(good).await.len(), 1, "not duplicated");
    assert_eq!(world.transactions(bad).await.len(), 1, "fetched again");
    assert_eq!(world.provider_account("bad").await.error, None);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// Records that could not be written are fetched again, as the engine tells
/// the member: the cursor does not pass them.
#[tokio::test]
async fn storage_failure_holds_the_cursor() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.server.inject_source_insert_failure().await;
    world.script.page(page(
        &[record(INJECTED_FAILURE_RECORD_ID, "acct-checking", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A currency coffer cannot hold never becomes usable by fetching again:
/// the account says so, and the others' cursor moves on.
#[tokio::test]
async fn unsupported_currency_does_not_hold_the_cursor() {
    let world = world().await;
    world.script.page(page(
        &[record("coin", "acct-crypto", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let crypto = world.provider_account("acct-crypto").await;
    assert!(crypto.error.unwrap().contains("currency"));
    assert_eq!(crypto.currency_raw, "BTC");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// Records of an account Plaid did not list become that account's error, and
/// are fetched again rather than lost.
#[tokio::test]
async fn records_of_an_unlisted_account_hold_the_cursor() {
    let world = world().await;
    world.script.page(page(
        &[record("stray", "acct-new", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        world
            .provider_account("acct-new")
            .await
            .error
            .unwrap()
            .contains("did not list")
    );
    assert_eq!(world.connection().await.provider_state, None);
}

/// A record that cannot be read never fails its account: it is counted, the
/// rest is applied, and the cursor moves on. One that names no account is
/// reported on the connection.
#[tokio::test]
async fn unusable_records_never_fail_the_account() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("fine", "acct-checking", "1"),
            record("quoted", "acct-checking", "\"12\""),
            json!({ "transaction_id": "no-fields", "account_id": "acct-checking" }),
            json!({ "transaction_id": "no-account" }),
        ],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail.unwrap().contains("2 skipped"),
        "both are counted"
    );
    assert_eq!(world.provider_account("acct-checking").await.error, None);
    assert_eq!(world.transactions(checking).await.len(), 1);
    let connection = world.connection().await;
    assert_eq!(connection.provider_state.as_deref(), Some("cursor-1"));
    assert!(
        connection
            .last_error
            .unwrap()
            .contains("assign to an account"),
        "a record of no known account is reported on the connection"
    );
}

/// Every Plaid text is sanitized before it is stored or shown.
#[tokio::test]
async fn provider_text_is_sanitized() {
    let world = world().await;
    let hostile = "Bank\u{202e}x https://user:pw-MARKER@h.example\u{7}";
    let account = world.bound("hostile").await;
    let mut listed = account_json("hostile", "depository", &json!(1));
    listed["name"] = json!(hostile);
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[listed, json!({ "account_id": "broken", "name": hostile })]),
    );
    let mut text = record("t", "hostile", "1");
    text["name"] = json!(hostile);
    text["merchant_name"] = json!(hostile);
    world.script.page(page(&[text], &[], &[], "c", false));

    world.sync().await;

    let mut stored = Vec::new();
    for id in ["hostile", "broken"] {
        stored.push(world.provider_account(id).await.name);
    }
    let transaction = world.transactions(account).await.remove(0);
    stored.push(transaction.raw_description.as_str().to_owned());
    stored.push(
        transaction
            .imported_payee
            .as_ref()
            .unwrap()
            .as_str()
            .to_owned(),
    );
    for text in stored {
        assert!(!text.contains("pw-MARKER"), "user information is hidden");
        assert!(!text.contains('\u{202e}'), "format characters are removed");
        assert!(!text.contains('\u{7}'), "control characters are removed");
    }
}

/// An Item whose first update is not ready answers with an empty cursor:
/// nothing is saved, and the next sync starts from the beginning.
#[tokio::test]
async fn empty_cursor_is_not_saved() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "", false));

    let run = world.sync().await;

    assert_ne!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A cursor is sent back as it came, so one that would be stored altered is
/// refused, and the saved cursor stays.
#[tokio::test]
async fn unusable_cursor_is_refused() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "u:p@host", false));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(
        run.detail.as_deref(),
        Some("Plaid sent a sync cursor coffer could not store")
    );
    assert_eq!(world.connection().await.provider_state, None);
}

/// An answer that never ends is cut off, with nothing stored.
#[tokio::test]
async fn endless_pagination_is_cut_off() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[record("again", "acct-checking", "1")],
            &[],
            &[],
            "same",
            true,
        ),
    );

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.script.cursors().len(), 200);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A passing failure at Plaid is `Unavailable`, which the connection shows as
/// an error it can retry; only Plaid's codes reach the text.
#[tokio::test]
async fn transient_answers_are_unavailable() {
    let world = world().await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::BAD_GATEWAY,
        "<html>bad gateway</html>",
    );

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(world.connection().await.status, ConnectionStatus::Error);
    let detail = run.detail.unwrap();
    assert!(detail.contains("HTTP_502"), "the status is named");
    assert!(!detail.contains("html"), "the body is not kept");
}

#[tokio::test]
async fn connect_seals_the_access_token_and_never_shows_it() {
    let world = world().await;
    let marker = log_marker();
    world.script.reply(
        EXCHANGE,
        StatusCode::OK,
        json!({ "access_token": "access-NEWTOKEN-MARKER-5", "item_id": "item-9" }).to_string(),
    );

    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::Plaid,
            ConnectionLabel::parse("New").unwrap(),
            ConnectInput::PlaidPublicToken(SecretString::from("public-PUBLICTOKEN-MARKER-6")),
            None,
        )
        .await
        .unwrap();

    let stored = world.server.store.get_connection(view.id).await.unwrap();
    assert_eq!(stored.provider, "plaid");
    let sealed = stored.credentials.unwrap();
    assert!(
        !String::from_utf8_lossy(&sealed).contains("NEWTOKEN-MARKER"),
        "the stored bytes do not hold the token in clear"
    );
    let opened = world.server.vault.open(view.id, &sealed.into()).unwrap();
    let credentials: Value = serde_json::from_str(opened.expose_secret()).unwrap();
    assert!(
        credentials["access_token"] == "access-NEWTOKEN-MARKER-5",
        "the credentials hold the access token"
    );
    assert!(
        credentials["item_id"] == "item-9",
        "the credentials hold the Item's id"
    );
    assert!(
        world.script.seen(EXCHANGE)[0]["public_token"] == "public-PUBLICTOKEN-MARKER-6",
        "the exchange names the public token"
    );
    let audit = format!("{:?}", world.audit().await);
    let view_text = serde_json::to_string(&view).unwrap();
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds this test");
    for text in [audit, view_text, log] {
        assert!(!text.contains("NEWTOKEN-MARKER"), "the token is not shown");
        assert!(
            !text.contains("PUBLICTOKEN-MARKER"),
            "the public token is not shown"
        );
    }
}

/// A sync's every outcome leaves the seeded access token out of the run, the
/// connection, the audit log and the logs.
#[tokio::test]
async fn access_token_absent_from_outcomes() {
    let world = world().await;
    let marker = log_marker();
    let echoed = format!("access-{ACCESS_MARKER}");
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": echoed, "error_code": echoed, "error_message": echoed }).to_string(),
    );
    let first = world.sync().await;
    world
        .script
        .reply(GET_ACCOUNTS, StatusCode::BAD_GATEWAY, echoed.clone());
    let second = world.sync().await;
    world.script.page(echoed.clone());
    let third = world.sync().await;

    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 3, "Plaid was asked");
    assert_eq!(world.script.seen(SYNC).len(), 1, "each answer was reached");
    for run in [&first, &second, &third] {
        assert_eq!(run.status, SyncRunStatus::Failed);
    }
    let connection = world.connection().await;
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds this test");
    for text in [
        format!("{first:?}"),
        format!("{second:?}"),
        format!("{third:?}"),
        format!("{:?}", connection.last_error),
        format!("{:?}", world.audit().await),
        log,
    ] {
        assert!(!text.contains(ACCESS_MARKER), "the token is not shown");
    }
}

/// New credentials would name another Item and leave the old one billing at
/// Plaid: a Plaid connection logs in again through update mode only, so no
/// input reconnects it, nothing reaches Plaid and the cursor stays.
#[tokio::test]
async fn reconnect_refuses_every_input_and_keeps_the_cursor() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "old-item", false));
    world.sync().await;
    let before = world.connection().await;

    for input in [
        ConnectInput::SimpleFinSetupToken(SecretString::from("x")),
        ConnectInput::PlaidPublicToken(SecretString::from("public-2")),
    ] {
        let refused = world
            .server
            .sync
            .reconnect(&world.alice, world.connection, input)
            .await;
        assert!(
            matches!(refused, Err(SyncError::Provider(ProviderError::Invalid(_)))),
            "update mode is the only way back"
        );
    }

    assert_eq!(
        world.script.seen(EXCHANGE).len(),
        0,
        "nothing was exchanged"
    );
    let after = world.connection().await;
    assert_eq!(after.provider_state.as_deref(), Some("old-item"));
    assert!(
        after.credentials == before.credentials,
        "the credentials are kept"
    );
}

/// `/item/remove` must not report success while the Item may keep billing: an
/// invalid token can also mean a wrong environment or wrong keys, and an Item
/// that needs a new login still exists.
#[tokio::test]
async fn removal_failures_leave_the_connection_unchanged() {
    for body in [
        json!({ "error_type": "INVALID_INPUT", "error_code": "INVALID_ACCESS_TOKEN" }).to_string(),
        LOGIN_REQUIRED.to_owned(),
        json!({ "error_type": "ITEM_ERROR", "error_code": "USER_PERMISSION_REVOKED" }).to_string(),
    ] {
        let world = world().await;
        let before = world.connection().await;
        world.script.reply(REMOVE, StatusCode::BAD_REQUEST, body);

        let (status, _) = world.delete_connection().await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let kept = world.connection().await;
        assert_eq!(kept.status, before.status);
        assert!(
            kept.credentials == before.credentials,
            "the credentials are kept"
        );
    }
}

/// Balances are valuations: one with more decimals than its currency applies
/// rounded half-even, and the raw field keeps the exact text.
#[tokio::test]
async fn too_precise_balance_is_rounded_and_keeps_its_text() {
    let world = world().await;
    let retirement = world.bound("acct-401k").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json("acct-401k", "investment", &json!(23631.9805))]),
    );
    world.script.page(page(
        &[record("r1", "acct-401k", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let balance = world
        .server
        .store
        .latest_balance(retirement)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(balance.balance_minor, 2_363_198);
    let provider_account = world.provider_account("acct-401k").await;
    assert_eq!(provider_account.error, None);
    assert_eq!(
        provider_account.last_balance.unwrap().amount_raw,
        "23631.9805"
    );
    assert_eq!(world.transactions(retirement).await.len(), 1);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// A balance too large to store is dropped and noted; the records still apply,
/// the cursor moves, and a later usable balance clears the note.
#[tokio::test]
async fn too_large_balance_is_noted_and_records_still_apply() {
    let world = world().await;
    let bound = world.bound("acct-big").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json(
            "acct-big",
            "depository",
            &json!(999_999_999_999_999_999_u64),
        )]),
    );
    world.script.page(page(
        &[record("r1", "acct-big", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    world.sync().await;

    assert_eq!(world.transactions(bound).await.len(), 1);
    assert!(
        world
            .server
            .store
            .latest_balance(bound)
            .await
            .unwrap()
            .is_none(),
        "the balance is dropped"
    );
    let noted = world.provider_account("acct-big").await;
    assert!(
        noted.error.unwrap().contains("balance"),
        "the provider account says why"
    );
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[account_json("acct-big", "depository", &json!(5))]),
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    assert_eq!(world.provider_account("acct-big").await.error, None);
}

/// A currency Plaid did not name may come with the next answer, so unlike an
/// unsupported one it holds the cursor, whatever its records say.
#[tokio::test]
async fn missing_currency_holds_the_cursor() {
    let world = world().await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-none","name":"None","type":"depository",
            "balances":{"current":5,"iso_currency_code":null,"unofficial_currency_code":null}}]}"#,
    );
    world.script.page(page(
        &[record("r1", "acct-none", "1")],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let held = world.provider_account("acct-none").await;
    assert!(held.error.unwrap().contains("currency"));
    assert_eq!(world.connection().await.provider_state, None);
}

/// An answer without a currency says nothing about the account's: the one a
/// good sync learned stays, and the refusal is recorded.
#[tokio::test]
async fn blank_currency_keeps_the_known_one() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    assert_eq!(
        world.provider_account("acct-checking").await.currency_raw,
        "USD"
    );

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-checking","name":"Checking","type":"depository",
            "balances":{"current":5,"iso_currency_code":null,"unofficial_currency_code":null}}]}"#,
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    let refused = world.provider_account("acct-checking").await;
    assert_eq!(refused.currency_raw, "USD", "the known currency stays");
    assert!(
        refused.error.unwrap().contains("currency"),
        "the refusal is recorded"
    );
}

/// A failed account's report says nothing about what it is: what a good sync
/// learned about it stays.
#[tokio::test]
async fn failed_account_keeps_what_is_known() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "cursor-1", false));
    world.sync().await;
    let known = world.provider_account("acct-card").await;
    assert_eq!(known.kind_hint, Some(AccountKind::CreditCard));

    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-card","name":"Other","type":"depository",
            "balances":{"current":1e2,"iso_currency_code":"EUR"}}]}"#,
    );
    world.script.page(page(&[], &[], &[], "cursor-2", false));
    world.sync().await;

    let failed = world.provider_account("acct-card").await;
    assert!(failed.error.is_some(), "the failure is recorded");
    assert_eq!(failed.name, known.name);
    assert_eq!(failed.currency_raw, known.currency_raw);
    assert_eq!(failed.kind_hint, known.kind_hint);
}

/// A dead Item cannot come back by trying again: scheduled syncs stop spending
/// requests on it, from either request of a fetch.
#[tokio::test]
async fn dead_item_marks_reauth() {
    for code in ["ITEM_NOT_FOUND", "USER_PERMISSION_REVOKED"] {
        for path in [GET_ACCOUNTS, SYNC] {
            let world = world().await;
            world.script.reply(
                path,
                StatusCode::BAD_REQUEST,
                json!({ "error_type": "ITEM_ERROR", "error_code": code }).to_string(),
            );

            let run = world.sync().await;

            assert_eq!(run.status, SyncRunStatus::Failed);
            assert_eq!(
                world.connection().await.status,
                ConnectionStatus::ReauthRequired
            );
        }
    }
}

/// Binding the account to another currency can be undone, so its records are
/// fetched again rather than passed by.
#[tokio::test]
async fn currency_differing_from_the_binding_holds_the_cursor() {
    let world = world().await;
    let bound = world.bound("acct-eur").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-eur","name":"Euro","type":"depository",
            "balances":{"current":5,"iso_currency_code":"EUR"}}]}"#,
    );
    let mut euros = record("r1", "acct-eur", "1");
    euros["iso_currency_code"] = json!("EUR");
    world
        .script
        .page(page(&[euros], &[], &[], "cursor-1", false));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.transactions(bound).await.len(), 0);
    assert_eq!(world.connection().await.provider_state, None);
}

/// A removal is a record too: a failed account holding only removals must see
/// them again.
#[tokio::test]
async fn failed_account_with_only_removals_holds_the_cursor() {
    let world = world().await;
    world.bound("acct-bad").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"acct-bad","name":"Bad","type":"depository",
            "balances":{"current":1e2,"iso_currency_code":"USD"}}]}"#,
    );
    world.script.page(page(
        &[],
        &[],
        &[json!({ "transaction_id": "gone", "account_id": "acct-bad" })],
        "cursor-1",
        false,
    ));

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert_eq!(world.connection().await.provider_state, None);
}

/// The stored text is what Plaid wrote, and a zero never gains a sign.
#[tokio::test]
async fn amount_text_is_exact_and_zero_is_unsigned() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::OK,
        accounts_answer(&[
            account_json("acct-checking", "depository", &json!(5)),
            account_json("acct-card", "credit", &json!(0)),
        ]),
    );
    world.script.page(page(
        &[
            record("zero", "acct-checking", "0"),
            record("cents", "acct-checking", "4.5"),
        ],
        &[],
        &[],
        "cursor-1",
        false,
    ));

    world.sync().await;

    let transactions = world.transactions(checking).await;
    let text = |description: &str| {
        transactions
            .iter()
            .find(|t| t.raw_description.as_str() == description)
            .unwrap()
            .sources[0]
            .amount_raw
            .clone()
    };
    assert_eq!(text("SHOP zero"), "0");
    assert_eq!(text("SHOP cents"), "4.5");
    assert_eq!(
        world
            .provider_account("acct-card")
            .await
            .last_balance
            .unwrap()
            .amount_raw,
        "0",
        "a zero balance owed gains no sign"
    );
}

/// Plaid is offered exactly when it is configured, with its documented
/// limits.
#[tokio::test]
async fn registered_only_when_configured() {
    let world = world().await;
    let config = Config {
        plaid: None,
        ..world.server.config.clone()
    };
    let without = ProviderRegistry::from_config(&config, &Egress::for_tests()).unwrap();
    assert!(without.get(ProviderKind::Plaid).is_none());

    let config = Config {
        plaid: Some(coffer_server::config::PlaidConfig {
            client_id: "id".to_owned(),
            secret: SecretString::from("secret"),
            environment: coffer_server::config::PlaidEnvironment::Sandbox,
            country_codes: vec!["US".to_owned()],
        }),
        ..world.server.config.clone()
    };
    let with = ProviderRegistry::from_config(&config, &Egress::for_tests()).unwrap();
    let plaid = with.get(ProviderKind::Plaid).unwrap();
    assert_eq!(plaid.kind(), ProviderKind::Plaid);
    let capabilities = plaid.capabilities();
    assert!(capabilities.pending);
    assert_eq!(capabilities.mode, FetchMode::Cursor);
    assert_eq!(capabilities.min_interval, SignedDuration::from_mins(15));
    assert_eq!(
        capabilities.min_scheduled_interval,
        SignedDuration::from_hours(1)
    );
    assert_eq!(capabilities.daily_budget, 48);
}

fn simplefin_record(description: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: "sf-1".into(),
        posted_on: Some(date(2027, 1, 10)),
        transacted_on: None,
        amount: DecimalAmount::parse("-1").unwrap(),
        amount_raw: "-1".to_owned(),
        description: description.into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

fn simplefin_answer(description: &str) -> FetchResult {
    FetchResult {
        accounts: vec![ProviderAccountData {
            provider_account_id: "sf-account".into(),
            name: "SimpleFin account".into(),
            institution: None,
            currency: "USD".into(),
            kind_hint: None,
            balance: None,
            transactions: vec![simplefin_record(description)],
            removed: Vec::new(),
            coverage: Coverage::Delta,
            error: None,
            unusable: Vec::new(),
        }],
        connection_errors: Vec::new(),
        cursor: None,
    }
}

/// One purchase reported by Plaid and by `SimpleFin` under different
/// descriptions: the provider that reported it first keeps owning the
/// imported fields, whatever the other sends, and alternating syncs write
/// nothing to the transaction.
#[tokio::test]
async fn alternating_providers_write_nothing_after_the_first() {
    let world = world().await;
    let account = world.bound("acct-checking").await;
    let simplefin = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "SimpleFin bank",
            world.alice.member,
            "https://u:p@bridge.example/simplefin",
        )
        .await;
    world.bind_to(simplefin, "sf-account", account).await;

    world.fake.push(Ok(simplefin_answer("ACME STORE 123")));
    world.sync_connection(simplefin).await;
    let mut plaid_record = record("pl-1", "acct-checking", "1");
    plaid_record["name"] = json!("Acme Store Purchase");
    world.script.page(page(
        std::slice::from_ref(&plaid_record),
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;

    let settled = world.transactions(account).await.remove(0);
    assert_eq!(
        settled.sources.len(),
        2,
        "both records hold one transaction"
    );
    assert_eq!(settled.raw_description.as_str(), "ACME STORE 123");
    let updates = world.imported_updates().await;

    for round in 0..3 {
        world.fake.push(Ok(simplefin_answer("ACME STORE 123")));
        world.sync_connection(simplefin).await;
        let mut changed = plaid_record.clone();
        changed["name"] = json!(format!("Acme Store Purchase {round}"));
        world
            .script
            .page(page(&[], std::slice::from_ref(&changed), &[], "c2", false));
        world.sync().await;
    }

    let after = world.transactions(account).await.remove(0);
    assert_eq!(after.raw_description, settled.raw_description);
    assert_eq!(
        after.updated_at, settled.updated_at,
        "nothing was rewritten"
    );
    assert_eq!(
        world.imported_updates().await,
        updates,
        "nothing was audited"
    );
    assert_eq!(world.transactions(account).await.len(), 1);
}
