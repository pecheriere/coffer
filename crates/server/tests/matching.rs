//! The apply step of a sync: provider records stored as source records and
//! matched onto canonical transactions without ever duplicating them
//! (spec §6.2, §8.4). Connections, provider accounts and bindings are made
//! with the store primitives.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, DisplayName, Role, Scope, TokenName, Username, Via};
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{
    CategoryKind, MISSING_DESCRIPTION, ReviewStatus, SplitInput, TransactionFlag, TransactionOrigin,
};
use coffer_server::http::router;
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::{NewManualTransactionInput, Patch, TransactionPatch};
use coffer_server::sync::apply::{
    AccountBatch, ApplyReport, Coverage, NormalizedTransaction, apply_account_batch, apply_held,
};
use coffer_server::testkit::TestServer;
use coffer_store::{
    AccountRecord, AuditEvent, AuditFilter, NewConnection, NewMember, PageRequest,
    ProviderAccountRecord, ProviderAccountUpsert, TransactionFilter, TransactionRecord,
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
}

/// A connection with one provider account, bound to `account` when set.
struct Link {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    account: Option<AccountRecord>,
}

async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let ledger = server.app_state().ledger;
    World {
        server,
        ledger,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

async fn account(world: &World) -> AccountRecord {
    let view = world
        .ledger
        .create_account(
            &world.alice,
            NewAccountInput {
                name: "Checking".into(),
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

/// A new connection and provider account, unbound.
async fn unbound_link(world: &World) -> Link {
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
    tx.commit().await.unwrap();
    Link {
        connection: connection.id,
        provider_account,
        account: None,
    }
}

async fn bind(world: &World, link: &mut Link, account: &AccountRecord) {
    let mut tx = world.server.store.begin().await.unwrap();
    link.provider_account = tx
        .bind_provider_account(link.provider_account.id, account.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    link.account = Some(account.clone());
}

/// A connection whose provider account is bound to `account`.
async fn bound_link(world: &World, account: &AccountRecord) -> Link {
    let mut link = unbound_link(world).await;
    bind(world, &mut link, account).await;
    link
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

fn window() -> Coverage {
    Coverage::Window {
        start: date(2026, 10, 1),
    }
}

async fn apply(
    world: &World,
    link: &Link,
    transactions: Vec<NormalizedTransaction>,
    removed: &[&str],
    coverage: Coverage,
) -> ApplyReport {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let report = apply_account_batch(
        &mut tx,
        now,
        link.connection,
        AccountBatch {
            provider_account: link.provider_account.clone(),
            account: link.account.clone(),
            transactions,
            removed: removed.iter().map(|id| (*id).to_owned()).collect(),
            coverage,
            unusable: Vec::new(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    report
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

/// `records` without when their sources were last seen, which every fetch
/// refreshes.
fn ignoring_last_seen(mut records: Vec<TransactionRecord>) -> Vec<TransactionRecord> {
    for source in records
        .iter_mut()
        .flat_map(|record| record.sources.iter_mut())
    {
        source.last_seen_at = source.first_seen_at;
    }
    records
}

async fn only_transaction(world: &World, account: AccountId) -> TransactionRecord {
    let mut all = transactions(world, account).await;
    assert_eq!(all.len(), 1);
    all.remove(0)
}

async fn transaction_audit(world: &World) -> Vec<AuditEvent> {
    world
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
        .items
}

async fn actions_for(world: &World, id: TransactionId) -> Vec<String> {
    let mut actions: Vec<String> = transaction_audit(world)
        .await
        .into_iter()
        .filter(|event| event.entity_id == id.to_string())
        .map(|event| event.action)
        .collect();
    actions.reverse();
    actions
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

fn splits(parts: &[(CategoryId, i64)]) -> Vec<SplitInput> {
    parts
        .iter()
        .map(|(category, minor_units)| SplitInput {
            category: *category,
            minor_units: *minor_units,
        })
        .collect()
}

async fn categorize(world: &World, id: TransactionId, parts: &[(CategoryId, i64)]) {
    world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                splits: Some(splits(parts)),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
}

fn advance(world: &World) {
    world
        .server
        .clock
        .advance(jiff::SignedDuration::from_secs(60));
}

#[tokio::test]
async fn reimport_is_idempotent() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let batch = || {
        vec![
            record("t1", 3, -1_250, "CORNER SHOP 0042", false),
            record("t2", 4, 250_000, "PAYROLL ACME", false),
            record("t3", 5, -499, "SQ *BLUE BOTTLE", true),
        ]
    };
    let first = apply(&world, &link, batch(), &[], window()).await;
    assert_eq!(first.created, 3);
    let before = transactions(&world, checking.id).await;
    let audit_before = transaction_audit(&world).await.len();
    assert_eq!(audit_before, 3, "one transaction.import each");
    advance(&world);

    let second = apply(&world, &link, batch(), &[], window()).await;
    assert_eq!(second, ApplyReport::default(), "nothing changed");
    assert_eq!(
        ignoring_last_seen(transactions(&world, checking.id).await),
        ignoring_last_seen(before)
    );
    assert_eq!(transaction_audit(&world).await.len(), audit_before);
}

#[tokio::test]
async fn user_state_survives_reimport() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let food = category(&world, "Food").await;
    let bob = {
        let mut tx = world.server.store.begin().await.unwrap();
        let bob = tx
            .insert_member(NewMember {
                id: MemberId::new(),
                username: Username::parse("bob").unwrap(),
                display_name: DisplayName::parse("Bob").unwrap(),
                role: Role::Member,
                created_at: world.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        bob.id
    };
    let mut original = record("t1", 3, -1_250, "CORNER SHOP 0042", false);
    original.payee = Some("Corner Shop".into());
    apply(&world, &link, vec![original], &[], window()).await;
    let id = only_transaction(&world, checking.id).await.id;
    world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Set("Neighbourhood shop".into()),
                notes: Patch::Set("milk and bread".into()),
                review: Some(ReviewStatus::Reviewed),
                assignee: Patch::Set(bob),
                splits: Some(splits(&[(food, -1_250)])),
            },
        )
        .await
        .unwrap();
    let edited = world.server.store.get_transaction(id).await.unwrap();
    advance(&world);

    let mut changed = record("t1", 4, -1_250, "CORNER SHOP #42 MAIN ST", false);
    changed.payee = Some("Corner Shop Inc".into());
    changed.memo = Some("card 1234".into());
    let report = apply(&world, &link, vec![changed], &[], window()).await;
    assert_eq!(report.updated, 1);

    let after = only_transaction(&world, checking.id).await;
    assert_eq!(after.id, id);
    assert_eq!(after.posted_on, date(2026, 10, 4));
    assert_eq!(after.raw_description.as_str(), "CORNER SHOP #42 MAIN ST");
    assert_eq!(
        after.imported_payee.as_ref().unwrap().as_str(),
        "Corner Shop Inc"
    );
    assert_eq!(after.memo.as_ref().unwrap().as_str(), "card 1234");
    assert_eq!(after.payee, edited.payee, "the edited payee stays");
    assert!(after.payee_edited);
    assert_eq!(after.notes, edited.notes);
    assert_eq!(after.review, ReviewStatus::Reviewed);
    assert_eq!(after.reviewed, edited.reviewed);
    assert_eq!(after.assignee, Some(bob));
    assert_eq!(after.splits, edited.splits);
    assert_eq!(after.categorized, edited.categorized);

    let update = transaction_audit(&world)
        .await
        .into_iter()
        .find(|event| event.action == "transaction.imported_update")
        .unwrap();
    assert_eq!(
        update.actor,
        AuditActor::System {
            reason: "system:sync".into()
        }
    );
    let fields: Vec<&str> = update.changes.iter().map(|(field, _)| field).collect();
    assert!(
        fields.contains(&"description"),
        "description must be among the changed fields"
    );
    assert!(!fields.contains(&"payee"), "the shown payee did not change");
}

#[tokio::test]
async fn pending_to_posted_keeps_transaction_id_without_user_state() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", true)],
        &[],
        window(),
    )
    .await;
    let pending = only_transaction(&world, checking.id).await;
    assert!(pending.pending);
    advance(&world);

    let report = apply(
        &world,
        &link,
        vec![record("posted-9", 12, -1_250, "CORNER SHOP 0042", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.attached, 1);
    assert_eq!(report.created, 0);

    let posted = only_transaction(&world, checking.id).await;
    assert_eq!(posted.id, pending.id, "same transaction id");
    assert!(!posted.pending);
    assert_eq!(posted.posted_on, date(2026, 10, 12));
    assert_eq!(posted.sources.len(), 1);
    assert_eq!(posted.sources[0].provider_txn_id, "posted-9");
    assert!(
        posted.flags.is_empty(),
        "the transaction must carry no flags"
    );
    let actions = actions_for(&world, posted.id).await;
    assert_eq!(
        actions,
        [
            "transaction.import",
            "transaction.attach_source",
            "transaction.detach_source"
        ]
    );
    let detach = transaction_audit(&world).await.remove(0);
    assert_eq!(
        detach.actor,
        AuditActor::System {
            reason: "system:sync".into()
        }
    );
    let (field, change) = detach.changes.iter().next().unwrap();
    assert_eq!(field, "source");
    assert_eq!(
        change.before,
        AuditValue::Text(pending.sources[0].id.to_string())
    );
    assert_eq!(change.after, AuditValue::Null);
}

#[tokio::test]
async fn pending_with_tip_attaches_to_posted() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let food = category(&world, "Food").await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -4_000, "BISTRO CENTRAL", true)],
        &[],
        window(),
    )
    .await;
    let pending = only_transaction(&world, checking.id).await;
    categorize(&world, pending.id, &[(food, -4_000)]).await;
    advance(&world);

    apply(
        &world,
        &link,
        vec![record("posted-1", 12, -4_600, "BISTRO CENTRAL 0099", false)],
        &[],
        window(),
    )
    .await;
    let posted = only_transaction(&world, checking.id).await;
    assert_eq!(
        posted.id, pending.id,
        "the tip did not create a transaction"
    );
    assert_eq!(posted.amount_minor, -4_600);
    assert_eq!(posted.splits.len(), 1);
    assert_eq!(posted.splits[0].category, food);
    assert_eq!(posted.splits[0].minor_units, -4_600, "the split follows");
    assert_eq!(
        posted.categorized.as_ref().unwrap().system.as_deref(),
        Some("system:sync")
    );
    assert_eq!(posted.sources.len(), 1);
    assert!(
        posted.flags.is_empty(),
        "the transaction must carry no flags"
    );
}

#[tokio::test]
async fn replaces_provider_id_attaches_to_vanished_source() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_000, "HOLD 1", true)],
        &[],
        Coverage::Delta,
    )
    .await;
    let pending = only_transaction(&world, checking.id).await;

    let mut posted = record("posted-1", 13, -2_000, "HARDWARE STORE", false);
    posted.replaces_provider_id = Some("pending-1".into());
    let report = apply(&world, &link, vec![posted], &["pending-1"], Coverage::Delta).await;
    assert_eq!(report.attached, 1);
    let after = only_transaction(&world, checking.id).await;
    assert_eq!(after.id, pending.id);
    assert_eq!(after.amount_minor, -2_000);
    assert_eq!(after.sources.len(), 1);
    assert_eq!(after.sources[0].provider_txn_id, "posted-1");
}

#[tokio::test]
async fn pending_record_never_reverts_a_posted_transaction() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let pending = record("pending-1", 10, -1_000, "BISTRO CENTRAL", true);
    apply(&world, &link, vec![pending.clone()], &[], Coverage::Delta).await;
    let mut posted = record("posted-1", 12, -1_150, "BISTRO CENTRAL", false);
    posted.replaces_provider_id = Some("pending-1".into());
    apply(&world, &link, vec![posted], &[], Coverage::Delta).await;
    let attached = only_transaction(&world, checking.id).await;
    assert_eq!(attached.sources.len(), 2);
    let audit_count = transaction_audit(&world).await.len();
    advance(&world);

    // The provider still reports the pending form.
    let report = apply(&world, &link, vec![pending], &[], Coverage::Delta).await;
    assert_eq!(report, ApplyReport::default());
    let after = only_transaction(&world, checking.id).await;
    assert!(!after.pending);
    assert_eq!(after.amount_minor, -1_150);
    assert_eq!(after.posted_on, date(2026, 10, 12));
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn pending_and_posted_both_present_flagged() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    apply(&world, &link, vec![pending.clone()], &[], window()).await;
    let pending_id = only_transaction(&world, checking.id).await.id;

    let both = vec![
        pending,
        record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
    ];
    let report = apply(&world, &link, both.clone(), &[], window()).await;
    assert_eq!(report.created, 1);
    assert_eq!(report.possible_duplicates, 1);
    let all = transactions(&world, checking.id).await;
    assert_eq!(all.len(), 2, "never silently merged");
    let posted = all.iter().find(|t| t.id != pending_id).unwrap();
    assert_eq!(posted.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(posted.duplicate_candidates, [pending_id]);
    let audit_count = transaction_audit(&world).await.len();

    apply(&world, &link, both, &[], window()).await;
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn identical_same_day_purchases_stay_distinct() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let batch = || {
        vec![
            record("a", 10, -500, "COFFEE CART", false),
            record("b", 10, -500, "COFFEE CART", false),
        ]
    };
    apply(&world, &link, batch(), &[], window()).await;
    let first = transactions(&world, checking.id).await;
    assert_eq!(first.len(), 2, "two purchases, two transactions");
    assert!(
        first
            .iter()
            .all(|t| t.flags.is_empty() && t.sources.len() == 1)
    );

    apply(&world, &link, batch(), &[], window()).await;
    assert_eq!(
        ignoring_last_seen(transactions(&world, checking.id).await),
        ignoring_last_seen(first),
        "still two"
    );
}

#[tokio::test]
async fn vanished_pending_without_state_deleted_with_state_flagged() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![
            record("p1", 10, -100, "BAKERY ONE", true),
            record("p2", 11, -200, "BAKERY TWO", true),
            record("old", 9, -300, "POSTED BEFORE", false),
        ],
        &[],
        window(),
    )
    .await;
    let all = transactions(&world, checking.id).await;
    let plain = all.iter().find(|t| t.amount_minor == -100).unwrap().id;
    let noted = all.iter().find(|t| t.amount_minor == -200).unwrap().id;
    let posted = all.iter().find(|t| t.amount_minor == -300).unwrap().id;
    world
        .ledger
        .update_transaction(
            &world.alice,
            noted,
            TransactionPatch {
                notes: Patch::Set("ask about this".into()),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    advance(&world);

    let report = apply(&world, &link, Vec::new(), &[], window()).await;
    assert_eq!(report.deleted, 1);
    assert_eq!(report.source_removed, 1);
    let left = transactions(&world, checking.id).await;
    assert_eq!(left.len(), 2, "a posted record absent from a window stays");
    assert!(left.iter().all(|t| t.id != plain), "deleted");
    let kept = left.iter().find(|t| t.id == noted).unwrap();
    assert_eq!(kept.flags, [TransactionFlag::SourceRemoved]);
    assert_eq!(kept.sources, Vec::new());
    assert!(left.iter().any(|t| t.id == posted));
    assert_eq!(
        actions_for(&world, plain).await,
        ["transaction.import", "transaction.delete"]
    );
    assert_eq!(
        actions_for(&world, noted).await.last().map(String::as_str),
        Some("transaction.source_removed")
    );

    // The record comes back: the kept transaction gains it and is unflagged.
    advance(&world);
    let report = apply(
        &world,
        &link,
        vec![record("p2", 11, -200, "BAKERY TWO", true)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.attached, 1);
    let back = world.server.store.get_transaction(noted).await.unwrap();
    assert!(back.flags.is_empty(), "the transaction must carry no flags");
    assert_eq!(back.sources.len(), 1);
}

#[tokio::test]
async fn multi_split_amount_change_flags_mismatch() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    apply(
        &world,
        &link,
        vec![record("t1", 10, -1_000, "SUPERSTORE", false)],
        &[],
        window(),
    )
    .await;
    let id = only_transaction(&world, checking.id).await.id;
    categorize(&world, id, &[(food, -600), (home, -400)]).await;
    let categorized = world.server.store.get_transaction(id).await.unwrap();
    advance(&world);

    apply(
        &world,
        &link,
        vec![record("t1", 10, -1_200, "SUPERSTORE", false)],
        &[],
        window(),
    )
    .await;
    let after = world.server.store.get_transaction(id).await.unwrap();
    assert_eq!(after.amount_minor, -1_200);
    assert_eq!(after.splits, categorized.splits, "several splits stay");
    assert_eq!(after.categorized, categorized.categorized);
    assert_eq!(after.flags, [TransactionFlag::SplitMismatch]);
    let update = transaction_audit(&world)
        .await
        .into_iter()
        .find(|event| event.action == "transaction.imported_update")
        .unwrap();
    let mismatch = update
        .changes
        .iter()
        .find(|(field, _)| *field == "split_mismatch")
        .unwrap()
        .1;
    assert_eq!(mismatch.after, AuditValue::Bool(true));
}

#[tokio::test]
async fn valid_split_replacement_clears_split_mismatch() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    apply(
        &world,
        &link,
        vec![record("t1", 10, -1_000, "SUPERSTORE", false)],
        &[],
        window(),
    )
    .await;
    let id = only_transaction(&world, checking.id).await.id;
    categorize(&world, id, &[(food, -600), (home, -400)]).await;
    apply(
        &world,
        &link,
        vec![record("t1", 10, -1_200, "SUPERSTORE", false)],
        &[],
        window(),
    )
    .await;
    advance(&world);

    let view = world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                splits: Some(splits(&[(food, -700), (home, -500)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(view.flags.len(), 0);
    let update = transaction_audit(&world).await.remove(0);
    assert_eq!(update.action, "transaction.update");
    let mismatch = update
        .changes
        .iter()
        .find(|(field, _)| *field == "split_mismatch")
        .unwrap()
        .1;
    assert_eq!(mismatch.before, AuditValue::Bool(true));
    assert_eq!(mismatch.after, AuditValue::Bool(false));
}

#[tokio::test]
async fn manual_transactions_never_matched() {
    let world = world().await;
    let checking = account(&world).await;
    let manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: checking.id,
                posted_on: date(2026, 10, 2),
                minor_units: -1_250,
                description: "FARMERS MARKET".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();
    let before = world.server.store.get_transaction(manual.id).await.unwrap();
    let link = bound_link(&world, &checking).await;

    let report = apply(
        &world,
        &link,
        vec![record("t1", 2, -1_250, "FARMERS MARKET", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.created, 1);
    let all = transactions(&world, checking.id).await;
    assert_eq!(all.len(), 2);
    let imported = all.iter().find(|t| t.id != manual.id).unwrap();
    assert_eq!(imported.origin, TransactionOrigin::Imported);
    assert_eq!(imported.flags, Vec::new());
    assert_eq!(
        world.server.store.get_transaction(manual.id).await.unwrap(),
        before,
        "the manual transaction is untouched"
    );
}

#[tokio::test]
async fn held_sources_matched_by_apply_held() {
    let world = world().await;
    let checking = account(&world).await;
    // Another provider already reports one of the purchases.
    let other = bound_link(&world, &checking).await;
    apply(
        &world,
        &other,
        vec![record("x-1", 10, -1_250, "CORNER SHOP", false)],
        &[],
        window(),
    )
    .await;
    let existing = only_transaction(&world, checking.id).await;

    let mut link = unbound_link(&world).await;
    let report = apply(
        &world,
        &link,
        vec![
            record("h1", 11, -1_250, "CORNER SHOP 42", false),
            record("h2", 12, -800, "BOOKSHOP", false),
        ],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.held, 2);
    assert_eq!(
        transactions(&world, checking.id).await.len(),
        1,
        "held, not imported"
    );

    bind(&world, &mut link, &checking).await;
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let report = apply_held(
        &mut tx,
        now,
        link.provider_account.clone(),
        checking.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        tx.held_sources(link.provider_account.id).await.unwrap(),
        Vec::new()
    );
    tx.commit().await.unwrap();
    assert_eq!(report.attached, 1, "matched across providers");
    assert_eq!(report.created, 1);

    let all = transactions(&world, checking.id).await;
    assert_eq!(all.len(), 2);
    let shared = all.iter().find(|t| t.id == existing.id).unwrap();
    assert_eq!(shared.sources.len(), 2, "one source per provider");
}

#[tokio::test]
async fn provider_text_is_sanitized_before_insert() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut long = record("t1", 10, -100, &"A".repeat(700), false);
    long.payee = Some(format!("Shop\u{0007}{}", "p".repeat(300)));
    long.memo = Some("line one\nline two".into());
    let mut empty = record("t2", 10, -200, "  \n ", false);
    empty.payee = Some("\t".into());
    empty.memo = Some(String::new());
    apply(&world, &link, vec![long, empty], &[], window()).await;

    let all = transactions(&world, checking.id).await;
    let long = all.iter().find(|t| t.amount_minor == -100).unwrap();
    assert_eq!(long.raw_description.as_str().chars().count(), 500);
    let payee = long.payee.as_ref().unwrap().as_str();
    assert_eq!(payee.chars().count(), 200);
    assert!(payee.starts_with("Shop p"));
    assert_eq!(long.memo.as_ref().unwrap().as_str(), "line one line two");
    assert_eq!(long.sources[0].fields.description, long.raw_description);

    let empty = all.iter().find(|t| t.amount_minor == -200).unwrap();
    assert_eq!(empty.raw_description.as_str(), MISSING_DESCRIPTION);
    assert_eq!(empty.payee, None);
    assert_eq!(empty.memo, None);
}

#[tokio::test]
async fn deleting_a_candidate_cascades() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    let posted = record("posted-1", 11, -1_250, "CORNER SHOP", false);
    apply(&world, &link, vec![pending.clone()], &[], window()).await;
    let pending_id = only_transaction(&world, checking.id).await.id;
    apply(&world, &link, vec![pending, posted.clone()], &[], window()).await;
    let flagged = transactions(&world, checking.id)
        .await
        .into_iter()
        .find(|t| t.id != pending_id)
        .unwrap();
    assert_eq!(flagged.duplicate_candidates, [pending_id]);

    // The pending record goes; its transaction, a candidate, is deleted.
    let report = apply(&world, &link, vec![posted], &["pending-1"], Coverage::Delta).await;
    assert_eq!(report.deleted, 1);
    let left = only_transaction(&world, checking.id).await;
    assert_eq!(left.id, flagged.id);
    assert!(left.duplicate_candidates.is_empty(), "the row went with it");
    assert_eq!(left.flags, Vec::new(), "no candidate is left to review");
    assert_eq!(
        actions_for(&world, left.id)
            .await
            .last()
            .map(String::as_str),
        Some("transaction.candidate_removed")
    );
}

/// Exactly two transactions, one flagged as a possible duplicate of the
/// other.
fn assert_flagged_pair(all: &[TransactionRecord]) {
    assert_eq!(all.len(), 2);
    let flagged: Vec<&TransactionRecord> = all
        .iter()
        .filter(|t| t.flags == [TransactionFlag::PossibleDuplicate])
        .collect();
    assert_eq!(flagged.len(), 1);
    let other = all.iter().find(|t| t.id != flagged[0].id).unwrap();
    assert_eq!(flagged[0].duplicate_candidates, [other.id]);
    assert_eq!(other.flags, Vec::new());
}

#[tokio::test]
async fn posted_then_pending_in_one_new_batch_flagged() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let batch = vec![
        record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
        record("pending-1", 10, -1_250, "CORNER SHOP", true),
    ];
    apply(&world, &link, batch, &[], window()).await;
    assert_flagged_pair(&transactions(&world, checking.id).await);
}

#[tokio::test]
async fn pending_then_posted_in_one_new_batch_flagged() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let batch = vec![
        record("pending-1", 10, -1_250, "CORNER SHOP", true),
        record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
    ];
    apply(&world, &link, batch, &[], window()).await;
    assert_flagged_pair(&transactions(&world, checking.id).await);
}

#[tokio::test]
async fn pending_reported_after_its_stored_posted_form_flagged() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let posted = record("posted-1", 11, -4_600, "BISTRO CENTRAL", false);
    apply(&world, &link, vec![posted.clone()], &[], window()).await;
    advance(&world);

    let pending = record("pending-1", 10, -4_000, "BISTRO CENTRAL", true);
    apply(&world, &link, vec![posted, pending], &[], window()).await;
    assert_flagged_pair(&transactions(&world, checking.id).await);
}

#[tokio::test]
async fn vanished_pending_beside_posted_unflags_the_posted() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    let posted = record("posted-1", 11, -1_250, "CORNER SHOP 0042", false);
    apply(&world, &link, vec![pending.clone()], &[], window()).await;
    apply(&world, &link, vec![pending, posted.clone()], &[], window()).await;
    assert_flagged_pair(&transactions(&world, checking.id).await);
    advance(&world);

    // The next fetch no longer reports the pending form.
    let report = apply(&world, &link, vec![posted], &[], window()).await;
    assert_eq!(report.deleted, 1);
    let left = only_transaction(&world, checking.id).await;
    assert!(!left.pending);
    assert_eq!(left.flags, Vec::new());
    assert_eq!(left.duplicate_candidates, Vec::new());
}

#[tokio::test]
async fn same_purchase_from_two_providers_is_one_transaction() {
    let world = world().await;
    let checking = account(&world).await;
    let first = bound_link(&world, &checking).await;
    let second = bound_link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase(), &[], window()).await;
    let report = apply(&world, &second, purchase(), &[], window()).await;
    assert_eq!(report.attached, 1);
    let shared = only_transaction(&world, checking.id).await;
    assert_eq!(shared.sources.len(), 2);
    assert_ne!(shared.sources[0].connection, shared.sources[1].connection);
    assert_eq!(shared.flags, Vec::new());
    let audit_count = transaction_audit(&world).await.len();
    advance(&world);

    assert_eq!(
        apply(&world, &first, purchase(), &[], window()).await,
        ApplyReport::default()
    );
    assert_eq!(
        apply(&world, &second, purchase(), &[], window()).await,
        ApplyReport::default()
    );
    assert_eq!(
        ignoring_last_seen(vec![only_transaction(&world, checking.id).await]),
        ignoring_last_seen(vec![shared])
    );
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn malformed_records_are_skipped() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut no_id = record("", 10, -100, "EMPTY ID", false);
    no_id.provider_id = String::new();
    let long_id = record(&"x".repeat(256), 10, -200, "LONG ID", false);
    let control_id = record("id\u{0000}1", 10, -300, "CONTROL ID", false);
    let mut bad_amount = record("bad-amount", 10, -400, "BAD AMOUNT", false);
    bad_amount.amount_raw = "12,50".into();
    let mut no_amount = record("no-amount", 10, -500, "NO AMOUNT", false);
    no_amount.amount_raw = String::new();
    let mut good = record(&"g".repeat(255), 10, -600, "GOOD", false);
    good.amount_raw = "-6.00".into();
    let report = apply(
        &world,
        &link,
        vec![no_id, long_id, control_id, bad_amount, no_amount, good],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.skipped, 5);
    assert_eq!(report.created, 1, "the account's other records still apply");
    assert_eq!(
        only_transaction(&world, checking.id).await.amount_minor,
        -600
    );
}

/// Sends `body` to `uri` with a fresh write token of alice's.
async fn call(
    world: &World,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .header(header::CONTENT_TYPE, "application/json");
    let request = match body {
        Some(body) => request.body(Body::from(body.to_string())).unwrap(),
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

#[tokio::test]
async fn linked_account_refuses_manual_transactions() {
    let world = world().await;
    let checking = account(&world).await;
    let path = format!("/api/v1/accounts/{}", checking.id);
    let (status, body) = call(&world, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "manual");
    let create = json!({"account": checking.id, "posted_on": "2026-10-02",
                        "minor_units": -500, "description": "Coffee"});
    let (status, _) = call(
        &world,
        Method::POST,
        "/api/v1/transactions",
        Some(create.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    bound_link(&world, &checking).await;
    assert!(
        world
            .server
            .store
            .account_is_linked(checking.id)
            .await
            .unwrap()
    );
    let (status, body) = call(&world, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "linked");
    let (status, body) = call(&world, Method::POST, "/api/v1/transactions", Some(create)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");

    let (status, body) = call(&world, Method::GET, "/api/v1/transactions", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "only the earlier manual one");
    assert_eq!(items[0]["sources"], json!([]));
    assert_eq!(items[0]["duplicate_candidates"], json!([]));
}

#[tokio::test]
async fn transaction_view_lists_its_sources() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    pending.amount_raw = "-12.50".into();
    apply(&world, &link, vec![pending], &[], window()).await;
    let id = only_transaction(&world, checking.id).await.id;

    let view = world
        .ledger
        .get_transaction(&world.alice, id)
        .await
        .unwrap();
    assert_eq!(view.sources.len(), 1);
    let source = &view.sources[0];
    assert_eq!(source.connection, link.connection);
    assert_eq!(source.provider_account, link.provider_account.id);
    assert!(source.pending);
    assert_eq!(source.posted_on, date(2026, 10, 10));
    assert_eq!(source.amount_raw, "-12.50");
    assert_eq!(view.duplicate_candidates, Vec::new());
}

#[tokio::test]
async fn malformed_rereport_of_a_known_record_is_not_vanished() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", true)],
        &[],
        window(),
    )
    .await;
    let stored = only_transaction(&world, checking.id).await;
    advance(&world);

    // The provider reports the same record again, in a shape we refuse.
    let mut reformatted = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    reformatted.amount_raw = "12,50".into();
    let report = apply(&world, &link, vec![reformatted], &[], window()).await;
    assert_eq!(report.skipped, 1);
    assert_eq!(report.deleted, 0);
    let kept = only_transaction(&world, checking.id).await;
    assert_eq!(kept.id, stored.id);
    assert_eq!(kept.flags, Vec::new());
    assert_eq!(kept.sources.len(), 1);
    assert_eq!(kept.sources[0].provider_txn_id, "pending-1");
}
