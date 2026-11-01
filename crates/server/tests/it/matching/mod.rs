//! The apply step of a sync: provider records stored as source records and
//! matched onto canonical transactions without ever duplicating them
//! (spec §6.2, §8.4). Connections, provider accounts and bindings are made
//! with the store primitives.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod ownership;
mod pending;
mod reimport;
mod split_amounts;

use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::sync::apply::{
    AccountBatch, ApplyReport, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_server::testkit::TestServer;
use coffer_store::{
    AccountRecord, AuditEvent, AuditFilter, NewConnection, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, TransactionFilter, TransactionRecord,
};
use jiff::civil::date;
use std::sync::Arc;

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
