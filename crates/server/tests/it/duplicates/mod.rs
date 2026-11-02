//! Resolving possible duplicates through the API: merging two imported
//! transactions, un-merging a source record into a transaction of its own,
//! and dismissing a flag (spec §6.2, §8.4). Imported transactions come from
//! the sync's apply step; connections and bindings are made with the store
//! primitives.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod dismissing;
mod merging;
mod unmerging;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, TransactionFlag};
use coffer_server::http::router;
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_server::testkit::TestServer;
use coffer_store::{
    AccountRecord, AuditEvent, AuditFilter, NewConnection, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, TransactionFilter, TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    ledger: Arc<Ledger>,
    alice: Actor,
    write_id: AgentTokenId,
    write: SecretToken,
}

/// A connection with one provider account bound to an account.
struct Link {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    account: AccountRecord,
}

async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let ledger = server.app_state().ledger;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (record, write) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("claude").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    World {
        server,
        ledger,
        alice,
        write_id: record.id,
        write,
    }
}

async fn account(world: &World, name: &str) -> AccountRecord {
    let view = world
        .ledger
        .create_account(
            &world.alice,
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
        .unwrap();
    world.server.store.get_account(view.id).await.unwrap()
}

/// A new connection whose provider account is bound to `account`.
async fn link(world: &World, account: &AccountRecord) -> Link {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let connection = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Bank").unwrap(),
            owner: world.alice.member,
            credentials: vec![1, 2, 3],
            created_at: now,
        })
        .await
        .unwrap();
    let provider_account = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: connection.id,
            provider_account_id: "acct-1".into(),
            name: "Everyday".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: Some(AccountKind::Checking),
            balance: None,
            seen_at: now,
        })
        .await
        .unwrap();
    let provider_account = tx
        .bind_provider_account(provider_account.id, account.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Link {
        connection: connection.id,
        provider_account,
        account: account.clone(),
    }
}

fn record(
    id: &str,
    day: i8,
    amount_minor: i64,
    description: &str,
    pending: bool,
) -> NormalizedTransaction {
    NormalizedTransaction {
        provider_id: id.into(),
        posted_on: date(2026, 10, day),
        date_reported: true,
        transacted_on: None,
        amount_minor,
        amount_raw: format!("{amount_minor}"),
        description: description.into(),
        payee: None,
        memo: None,
        pending,
        replaces_provider_id: None,
    }
}

/// Applies `transactions` as a delta fetch: nothing vanishes.
async fn apply(world: &World, link: &Link, transactions: Vec<NormalizedTransaction>) {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    apply_account_batch(
        &mut tx,
        now,
        link.connection,
        AccountBatch {
            provider_account: link.provider_account.clone(),
            account: Some(link.account.clone()),
            transactions,
            removed: Vec::new(),
            vanishing: HashSet::new(),
            coverage: Coverage::Delta,
            unusable: Vec::new(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// The account's transactions, oldest first.
async fn transactions(world: &World, account: AccountId) -> Vec<TransactionRecord> {
    let mut items = world
        .server
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
            &PageRequest {
                after: None,
                limit: 200,
            },
        )
        .await
        .unwrap()
        .items;
    items.reverse();
    items
}

/// The transaction holding the record `provider_id` of `link`.
async fn holding(world: &World, link: &Link, provider_id: &str) -> TransactionRecord {
    transactions(world, link.account.id)
        .await
        .into_iter()
        .find(|t| {
            t.sources.iter().any(|source| {
                source.connection == link.connection && source.provider_txn_id == provider_id
            })
        })
        .unwrap()
}

async fn get(world: &World, id: TransactionId) -> TransactionRecord {
    world.server.store.get_transaction(id).await.unwrap()
}

/// Every transaction audit event, oldest first.
async fn transaction_audit(world: &World) -> Vec<AuditEvent> {
    let mut items = world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("transaction".into()),
                ..AuditFilter::default()
            },
            &PageRequest {
                after: None,
                limit: 200,
            },
        )
        .await
        .unwrap()
        .items;
    items.reverse();
    items
}

/// The latest audit event of `action` on `id`.
async fn last_event(world: &World, id: TransactionId, action: &str) -> AuditEvent {
    transaction_audit(world)
        .await
        .into_iter()
        .rev()
        .find(|event| event.entity_id == id.to_string() && event.action == action)
        .unwrap()
}

fn before(event: &AuditEvent, field: &str) -> AuditValue {
    event
        .changes
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, change)| change.before.clone())
        .unwrap()
}

async fn category(world: &World, name: &str) -> CategoryId {
    let group = world
        .ledger
        .create_category_group(
            &world.alice,
            CategoryGroupInput {
                name: format!("{name} group"),
                position: None,
            },
            None,
        )
        .await
        .unwrap();
    world
        .ledger
        .create_category(
            &world.alice,
            CategoryInput {
                group: group.id,
                name: name.into(),
                kind: CategoryKind::Expense,
                position: None,
            },
            None,
        )
        .await
        .unwrap()
        .id
}

async fn patch(world: &World, id: TransactionId, patch: TransactionPatch) {
    world
        .ledger
        .update_transaction(&world.alice, id, patch)
        .await
        .unwrap();
}

async fn send(
    world: &World,
    token: &SecretToken,
    uri: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(Method::POST).uri(uri).header(
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
    let response = router(world.server.app_state())
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

async fn merge(world: &World, target: TransactionId, body: Value) -> (StatusCode, Value) {
    send(
        world,
        &world.write,
        &format!("/api/v1/transactions/{target}/merge"),
        Some(body),
        None,
    )
    .await
}

async fn unmerge(
    world: &World,
    id: TransactionId,
    source: impl std::fmt::Display,
    key: Option<&str>,
) -> (StatusCode, Value) {
    send(
        world,
        &world.write,
        &format!("/api/v1/transactions/{id}/unmerge"),
        Some(json!({ "source": source.to_string() })),
        key,
    )
    .await
}

async fn dismiss(world: &World, id: TransactionId) -> (StatusCode, Value) {
    send(
        world,
        &world.write,
        &format!("/api/v1/transactions/{id}/dismiss-duplicate"),
        None,
        None,
    )
    .await
}

fn by_write_token(world: &World, event: &AuditEvent) {
    assert_eq!(
        event.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(world.write_id),
        }
    );
}

/// A pending record, then that record beside a posted one of the same
/// amount: the posted one is flagged with the pending one as candidate.
/// Returns (pending, posted).
async fn flagged_pair(world: &World, link: &Link) -> (TransactionId, TransactionId) {
    let pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    apply(world, link, vec![pending.clone()]).await;
    let posted = record("posted-1", 11, -1_250, "CORNER SHOP 0042", false);
    apply(world, link, vec![pending, posted]).await;
    let pending = holding(world, link, "pending-1").await;
    let posted = holding(world, link, "posted-1").await;
    assert_eq!(posted.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(posted.duplicate_candidates, [pending.id]);
    (pending.id, posted.id)
}

/// One posted record of `amount` from each link, each its own transaction.
async fn one_each(world: &World, links: &[&Link], amounts: &[i64]) -> Vec<TransactionId> {
    let mut ids = Vec::new();
    for (link, amount) in links.iter().zip(amounts) {
        apply(
            world,
            link,
            vec![record("p-1", 10, *amount, "HARDWARE", false)],
        )
        .await;
        ids.push(holding(world, link, "p-1").await.id);
    }
    ids
}

/// Flags `transaction` with `candidates`, as the sync would.
async fn flag_with(world: &World, transaction: TransactionId, candidates: &[TransactionId]) {
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_flag(transaction, TransactionFlag::PossibleDuplicate, true)
        .await
        .unwrap();
    tx.add_duplicate_candidates(transaction, candidates)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
