//! Resolving possible duplicates through the API: merging two imported
//! transactions, un-merging a source record into a transaction of its own,
//! and dismissing a flag (spec §6.2, §8.4). Imported transactions come from
//! the sync's apply step; connections and bindings are made with the store
//! primitives.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, SplitInput, TransactionFlag};
use coffer_server::http::router;
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::{NewManualTransactionInput, Patch, TransactionPatch};
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

#[tokio::test]
async fn merge_moves_sources_and_refuses_user_state() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let groceries = category(&world, "Groceries").await;
    // The target's user state, which the merge keeps.
    patch(
        &world,
        posted,
        TransactionPatch {
            notes: Patch::Set("weekly shop".into()),
            splits: Some(vec![SplitInput {
                category: groceries,
                minor_units: -1_250,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    // The duplicate's, which a merge refuses to drop unless told to.
    patch(
        &world,
        pending,
        TransactionPatch {
            notes: Patch::Set("from the receipt".into()),
            ..TransactionPatch::default()
        },
    )
    .await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, posted, json!({ "duplicate": pending })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "merge_conflict");
    assert!(!body.to_string().contains(&pending.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);

    let (status, body) = merge(
        &world,
        posted,
        json!({ "duplicate": pending, "discard_duplicate_state": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], posted.to_string());
    let merged = get(&world, posted).await;
    assert_eq!(
        transactions(&world, checking.id).await,
        std::slice::from_ref(&merged)
    );
    let mut providers: Vec<&str> = merged
        .sources
        .iter()
        .map(|source| source.provider_txn_id.as_str())
        .collect();
    providers.sort_unstable();
    assert_eq!(providers, ["pending-1", "posted-1"]);
    assert_eq!(merged.notes.unwrap().as_str(), "weekly shop");
    assert_eq!(merged.splits.len(), 1);
    assert!(!merged.pending, "the posted target keeps its fields");
    assert_eq!(merged.flags, Vec::new());
    assert_eq!(merged.duplicate_candidates, Vec::new());

    let discarded = last_event(&world, pending, "transaction.merged").await;
    by_write_token(&world, &discarded);
    assert_eq!(
        before(&discarded, "notes"),
        AuditValue::Text("from the receipt".into())
    );
    let event = last_event(&world, posted, "transaction.merge").await;
    by_write_token(&world, &event);
    assert_eq!(before(&event, "possible_duplicate"), AuditValue::Bool(true));

    // The merged records stay on the target through later syncs.
    let audit_count = transaction_audit(&world).await.len();
    apply(
        &world,
        &link,
        vec![
            record("pending-1", 10, -1_250, "CORNER SHOP", true),
            record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
        ],
    )
    .await;
    assert_eq!(transactions(&world, checking.id).await.len(), 1);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merge_takes_the_posted_fields_of_a_posted_duplicate() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let groceries = category(&world, "Groceries").await;
    patch(
        &world,
        pending,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: groceries,
                minor_units: -1_250,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;

    // The pending transaction holds the user state: it is the target.
    let (status, _) = merge(&world, pending, json!({ "duplicate": posted })).await;
    assert_eq!(status, StatusCode::OK);
    let merged = get(&world, pending).await;
    assert!(!merged.pending);
    assert_eq!(merged.posted_on, date(2026, 10, 11));
    assert_eq!(merged.raw_description.as_str(), "CORNER SHOP 0042");
    assert_eq!(merged.splits.len(), 1, "the target's splits stay");
    assert_eq!(merged.sources.len(), 2);
    assert_eq!(merged.flags, Vec::new());
}

#[tokio::test]
async fn merge_across_accounts_refused() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let savings = account(&world, "Savings").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &savings).await;
    let ids = one_each(&world, &[&first, &second], &[-500, -500]).await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, ids[0], json!({ "duplicate": ids[1] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid");
    assert_eq!(get(&world, ids[1]).await.sources.len(), 1);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merge_refuses_itself_manual_and_unknown_transactions() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let ids = one_each(&world, &[&link], &[-500]).await;
    let cash = account(&world, "Cash").await;
    let manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: cash.id,
                posted_on: date(2026, 10, 10),
                minor_units: -500,
                description: "Market".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();
    let other_manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: cash.id,
                posted_on: date(2026, 10, 10),
                minor_units: -500,
                description: "Market".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();

    let (status, _) = merge(&world, ids[0], json!({ "duplicate": ids[0] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = merge(&world, manual.id, json!({ "duplicate": other_manual.id })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = TransactionId::new();
    let (status, body) = merge(&world, ids[0], json!({ "duplicate": unknown })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_string().contains(&unknown.to_string()));
    let (status, _) = merge(&world, unknown, json!({ "duplicate": ids[0] })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn merge_clears_flag_when_last_candidate_goes() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let links = [
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
    ];
    let ids = one_each(&world, &[&links[0], &links[1], &links[2]], &[-1, -2, -3]).await;
    let (target, first, second) = (ids[0], ids[1], ids[2]);
    flag_with(&world, target, &[first, second]).await;

    let (status, _) = merge(&world, target, json!({ "duplicate": first })).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, target).await;
    assert_eq!(after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(after.duplicate_candidates, [second]);

    let (status, body) = merge(&world, target, json!({ "duplicate": second })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["flags"], json!([]));
    let after = get(&world, target).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(after.sources.len(), 3);
    let event = last_event(&world, target, "transaction.merge").await;
    assert_eq!(before(&event, "possible_duplicate"), AuditValue::Bool(true));
}

#[tokio::test]
async fn merge_carries_the_duplicates_candidacies_to_the_target() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let links = [
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
    ];
    let fifth = link(&world, &checking).await;
    let ids = one_each(
        &world,
        &[&links[0], &links[1], &links[2], &links[3], &fifth],
        &[-1, -2, -3, -4, -5],
    )
    .await;
    let (target, duplicate, candidate, lister) = (ids[0], ids[1], ids[2], ids[3]);
    let both = ids[4];
    // The duplicate may itself duplicate `candidate`, and `lister` may
    // duplicate it: both now concern the target. `both` already listed the
    // target: it only loses the duplicate.
    flag_with(&world, duplicate, &[target, candidate]).await;
    flag_with(&world, lister, &[duplicate]).await;
    flag_with(&world, both, &[duplicate, target]).await;

    let (status, _) = merge(&world, target, json!({ "duplicate": duplicate })).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, target).await;
    assert_eq!(after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(after.duplicate_candidates, [candidate]);
    let lister_after = get(&world, lister).await;
    assert_eq!(lister_after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(lister_after.duplicate_candidates, [target]);
    by_write_token(
        &world,
        &last_event(&world, lister, "transaction.candidate_replaced").await,
    );
    assert_eq!(get(&world, both).await.duplicate_candidates, [target]);
    let removed = last_event(&world, both, "transaction.candidate_removed").await;
    by_write_token(&world, &removed);
    assert_eq!(
        before(&removed, "duplicate_candidate"),
        AuditValue::Text(duplicate.to_string())
    );
    assert!(
        !transaction_audit(&world).await.iter().any(|event| {
            event.entity_id == both.to_string() && event.action == "transaction.candidate_replaced"
        }),
        "nothing was replaced"
    );
}

#[tokio::test]
async fn merge_drops_candidates_the_same_provider_reported_apart() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    // Two purchases the second provider reports apart, and one from the
    // first provider that matches both: flagged as ambiguous.
    apply(
        &world,
        &second,
        vec![
            record("b-1", 10, -2_999, "HARDWARE STORE", false),
            record("b-2", 10, -2_999, "HARDWARE STORE", false),
        ],
    )
    .await;
    apply(
        &world,
        &first,
        vec![record("a-1", 10, -2_999, "HARDWARE STORE", false)],
    )
    .await;
    let flagged = holding(&world, &first, "a-1").await;
    let one = holding(&world, &second, "b-1").await.id;
    let two = holding(&world, &second, "b-2").await.id;
    assert_eq!(flagged.duplicate_candidates.len(), 2);

    let (status, _) = merge(&world, flagged.id, json!({ "duplicate": one })).await;
    assert_eq!(status, StatusCode::OK);
    // The target now holds b-1, which the second provider reported apart
    // from b-2: it cannot also be b-2's purchase.
    let after = get(&world, flagged.id).await;
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(after.flags, Vec::new());
    let event = last_event(&world, flagged.id, "transaction.candidate_removed").await;
    by_write_token(&world, &event);
    assert_eq!(
        before(&event, "duplicate_candidate"),
        AuditValue::Text(two.to_string())
    );
}

#[tokio::test]
async fn candidate_turning_posted_clears_stale_flag() {
    let world = world().await;
    // The pending candidate attaches its posted form (a record naming the
    // one it replaces): the flagged posted purchase is another one.
    let checking = account(&world, "Checking").await;
    let attaching = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &attaching).await;
    let mut replacement = record("final-1", 11, -1_250, "CORNER SHOP", false);
    replacement.replaces_provider_id = Some("pending-1".into());
    apply(&world, &attaching, vec![replacement]).await;
    assert_eq!(get(&world, pending).await.sources.len(), 2);
    let after = get(&world, posted).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
    let event = last_event(&world, posted, "transaction.candidate_removed").await;
    assert_eq!(
        event.actor,
        AuditActor::System {
            reason: "system:sync".into()
        }
    );

    // The pending record itself is re-reported as posted.
    let savings = account(&world, "Savings").await;
    let in_place = link(&world, &savings).await;
    let (_, posted) = flagged_pair(&world, &in_place).await;
    apply(
        &world,
        &in_place,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", false)],
    )
    .await;
    let after = get(&world, posted).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
}

#[tokio::test]
async fn unmerge_restores_separate_transaction() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    let shared = holding(&world, &first, "p-1").await;
    assert_eq!(shared.sources.len(), 2);
    let tools = category(&world, "Tools").await;
    patch(
        &world,
        shared.id,
        TransactionPatch {
            notes: Patch::Set("drill".into()),
            splits: Some(vec![SplitInput {
                category: tools,
                minor_units: -2_999,
            }]),
            review: Some(coffer_domain::transactions::ReviewStatus::Reviewed),
            ..TransactionPatch::default()
        },
    )
    .await;
    let source = shared
        .sources
        .iter()
        .find(|source| source.connection == second.connection)
        .unwrap()
        .id;

    let (status, body) = unmerge(&world, shared.id, source, None).await;
    assert_eq!(status, StatusCode::CREATED);
    let created: TransactionId = body["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(created, shared.id);
    let split_off = get(&world, created).await;
    assert_eq!(split_off.sources.len(), 1);
    assert_eq!(split_off.sources[0].id, source);
    assert_eq!(split_off.amount_minor, -2_999);
    assert!(
        split_off.user_state().is_empty(),
        "the split-off transaction must carry no user state"
    );
    assert_eq!(split_off.flags, Vec::new());
    let kept = get(&world, shared.id).await;
    assert_eq!(kept.sources.len(), 1);
    assert_eq!(kept.sources[0].connection, first.connection);
    assert_eq!(kept.notes.unwrap().as_str(), "drill");
    assert_eq!(kept.splits.len(), 1);
    by_write_token(
        &world,
        &last_event(&world, shared.id, "transaction.unmerge").await,
    );
    by_write_token(
        &world,
        &last_event(&world, created, "transaction.unmerged").await,
    );

    // Later syncs keep them apart.
    let audit_count = transaction_audit(&world).await.len();
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn unmerge_recomputes_the_kept_transactions_fields() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    // A pending charge, then its posted form with a tip, attached.
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -4_000, "BISTRO", true)],
    )
    .await;
    let mut posted = record("posted-1", 11, -4_600, "BISTRO", false);
    posted.replaces_provider_id = Some("pending-1".into());
    apply(&world, &link, vec![posted]).await;
    let joined = holding(&world, &link, "pending-1").await;
    assert_eq!(joined.amount_minor, -4_600);
    let dining = category(&world, "Dining").await;
    patch(
        &world,
        joined.id,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: dining,
                minor_units: -4_600,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    let posted_source = joined
        .sources
        .iter()
        .find(|s| !s.fields.pending)
        .unwrap()
        .id;

    let (status, body) = unmerge(&world, joined.id, posted_source, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["amount"]["minor_units"], -4_600);
    assert_eq!(body["pending"], false);
    let kept = get(&world, joined.id).await;
    assert!(kept.pending, "only its pending record is left");
    assert_eq!(kept.amount_minor, -4_000);
    assert_eq!(kept.posted_on, date(2026, 10, 10));
    assert_eq!(kept.splits[0].minor_units, -4_000, "a single split follows");
}

#[tokio::test]
async fn unmerge_with_idempotency_key_replays() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    let shared = holding(&world, &first, "p-1").await;
    let source = shared.sources[1].id;

    let (status, body) = unmerge(&world, shared.id, source, Some("split-1")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (again, replayed) = unmerge(&world, shared.id, source, Some("split-1")).await;
    assert_eq!(again, StatusCode::CREATED);
    assert_eq!(replayed["id"], body["id"]);
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
}

#[tokio::test]
async fn unmerge_last_source_refused() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let ids = one_each(&world, &[&link], &[-500]).await;
    apply(&world, &link, vec![record("p-2", 11, -700, "OTHER", false)]).await;
    let other = holding(&world, &link, "p-2").await;
    let only = get(&world, ids[0]).await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = unmerge(&world, only.id, only.sources[0].id, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");
    // A record of another transaction is not one of its own.
    let (status, body) = unmerge(&world, only.id, other.sources[0].id, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!body.to_string().contains(&other.sources[0].id.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn dismiss_clears_flag_and_candidates() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;

    let (status, body) = dismiss(&world, posted).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["flags"], json!([]));
    assert_eq!(body["duplicate_candidates"], json!([]));
    let after = get(&world, posted).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    let event = last_event(&world, posted, "transaction.dismiss_duplicate").await;
    by_write_token(&world, &event);
    assert_eq!(
        before(&event, "duplicate_candidates"),
        AuditValue::Text(pending.to_string())
    );

    // Dismissing again changes and records nothing; a later sync does not
    // flag it again.
    let audit_count = transaction_audit(&world).await.len();
    let (status, _) = dismiss(&world, posted).await;
    assert_eq!(status, StatusCode::OK);
    apply(
        &world,
        &link,
        vec![
            record("pending-1", 10, -1_250, "CORNER SHOP", true),
            record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
        ],
    )
    .await;
    assert_eq!(get(&world, posted).await.flags, Vec::new());
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn read_token_cannot_merge() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let (_, read) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let source = get(&world, posted).await.sources[0].id;
    let audit_count = transaction_audit(&world).await.len();

    for (uri, body) in [
        (
            format!("/api/v1/transactions/{posted}/merge"),
            Some(json!({ "duplicate": pending })),
        ),
        (
            format!("/api/v1/transactions/{posted}/merge"),
            Some(json!({ "duplicate": posted })),
        ),
        (
            format!("/api/v1/transactions/{posted}/unmerge"),
            Some(json!({ "source": source })),
        ),
        (
            format!("/api/v1/transactions/{posted}/dismiss-duplicate"),
            None,
        ),
    ] {
        let (status, problem) = send(&world, &read, &uri, body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(problem["code"], "forbidden");
    }
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merged_records_of_two_connections_stay_put_through_resyncs() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    // The same purchase, reported differently enough not to match.
    let one = || vec![record("a-1", 10, -2_999, "HARDWARE STORE", false)];
    let other = || vec![record("b-1", 13, -3_010, "HDW STORE 12 CITY", false)];
    apply(&world, &first, one()).await;
    apply(&world, &second, other()).await;
    let target = holding(&world, &first, "a-1").await.id;
    let duplicate = holding(&world, &second, "b-1").await.id;
    let tools = category(&world, "Tools").await;
    patch(
        &world,
        target,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: tools,
                minor_units: -2_999,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    let (status, _) = merge(&world, target, json!({ "duplicate": duplicate })).await;
    assert_eq!(status, StatusCode::OK);
    let merged = get(&world, target).await;
    let audit_count = transaction_audit(&world).await.len();

    for _ in 0..2 {
        world
            .server
            .clock
            .advance(jiff::SignedDuration::from_secs(60));
        apply(&world, &first, one()).await;
        apply(&world, &second, other()).await;
    }
    let after = get(&world, target).await;
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
    assert_eq!(after.amount_minor, merged.amount_minor);
    assert_eq!(after.posted_on, merged.posted_on);
    assert_eq!(after.raw_description, merged.raw_description);
    assert_eq!(after.splits, merged.splits);
    assert_eq!(after.updated_at, merged.updated_at);
}

#[tokio::test]
async fn merge_refuses_purchases_one_connection_reported_apart() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![
            record("p-1", 10, -500, "MARKET", false),
            record("p-2", 10, -700, "MARKET", false),
        ],
    )
    .await;
    let one = holding(&world, &link, "p-1").await.id;
    let two = holding(&world, &link, "p-2").await.id;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, one, json!({ "duplicate": two })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid");
    assert!(!body.to_string().contains(&two.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn dismiss_drops_the_reverse_link_of_a_mutual_pair() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    let third = link(&world, &checking).await;
    let ids = one_each(&world, &[&first, &second, &third], &[-1, -2, -3]).await;
    let (dismissed, mutual, other) = (ids[0], ids[1], ids[2]);
    flag_with(&world, dismissed, &[mutual]).await;
    flag_with(&world, mutual, &[dismissed, other]).await;

    let (status, _) = dismiss(&world, dismissed).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, mutual).await;
    assert_eq!(after.duplicate_candidates, [other], "its other pair stays");
    assert_eq!(after.flags, [TransactionFlag::PossibleDuplicate]);
    let event = last_event(&world, mutual, "transaction.candidate_removed").await;
    by_write_token(&world, &event);
    assert_eq!(
        before(&event, "duplicate_candidate"),
        AuditValue::Text(dismissed.to_string())
    );

    // Dismissing `other`'s only lister side leaves `mutual` with no
    // candidate once its last pair goes the same way.
    flag_with(&world, other, &[mutual]).await;
    let (status, _) = dismiss(&world, other).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, mutual).await;
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(after.flags, Vec::new());
}
