//! Syncing from recorded fixtures: state, dates, amounts and error mapping.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{
    ACCOUNTS_ERRORS, ACCOUNTS_FIRST, ACCOUNTS_POSTED, Reply, World, account_set, world,
    world_with_env,
};
use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::CategoryId;
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_store::{AuditFilter, ConnectionStatus, PageRequest};
use jiff::civil::date;
use serde_json::json;

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
    world.script.bridge_reply(Reply::ok(ACCOUNTS_FIRST));
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

    world.script.bridge_reply(Reply::ok(ACCOUNTS_POSTED));
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
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
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
    world.script.bridge_reply(Reply::ok(ACCOUNTS_ERRORS));

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
    let savings = world.provider_account("ACT-0004").await;
    assert_eq!(savings.name, "Savings");
    assert_eq!(
        savings.error, None,
        "a record without an amount is skipped, not the account's error"
    );
    assert!(savings.last_balance.is_some());
    assert!(
        run.detail.as_deref().unwrap().contains("1 skipped"),
        "the record without an amount is skipped"
    );
}

/// An account whose own fields are unusable (here its balance) is the
/// account's error, and its name is still recorded.
#[tokio::test]
async fn unreadable_account_is_its_error() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(
        &json!({
            "errors": [],
            "accounts": [{
                "id": "ACT-0009", "name": "Brokerage", "currency": "USD",
                "balance": "lots", "balance-date": 1_799_992_800, "transactions": [],
            }],
        })
        .to_string(),
    ));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let unreadable = world.provider_account("ACT-0009").await;
    assert_eq!(unreadable.name, "Brokerage");
    assert!(unreadable.error.is_some());
}

/// A record without an amount, with an unparsable amount, a timestamp out
/// of range or no id is skipped; the account's other records and its
/// balance are applied.
#[tokio::test]
async fn unusable_records_skipped_others_applied() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([
        { "id": "TRN-0001", "posted": 1_799_745_300, "amount": "-5.00",
          "description": "PARKING METER" },
        { "id": "TRN-0002", "posted": 1_799_745_300, "description": "NO AMOUNT" },
        { "id": "TRN-0003", "posted": 1_799_745_300, "amount": "12,50",
          "description": "BAD AMOUNT" },
        { "id": "TRN-0004", "posted": i64::MAX, "amount": "-1.00",
          "description": "FAR FUTURE" },
        { "posted": 1_799_745_300, "amount": "-2.00", "description": "NO ID" },
        { "id": "TRN-0005", "posted": 1_799_745_300, "amount": "-7.25",
          "description": "BOOKSHOP" },
    ]))));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        run.detail.as_deref().unwrap().contains("4 skipped"),
        "every unusable record is counted as skipped"
    );
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.error, None, "the account itself is not in error");
    assert!(held.last_balance.is_some(), "the balance is applied");
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    let mut amounts: Vec<i64> = world
        .transactions(account)
        .await
        .iter()
        .map(|txn| txn.amount_minor)
        .collect();
    amounts.sort_unstable();
    assert_eq!(amounts, vec![-725, -500]);
}

#[tokio::test]
async fn reauth_on_403() {
    let world = world().await;
    let connection = world.seeded().await;
    world
        .script
        .bridge_reply(Reply::status(StatusCode::FORBIDDEN, "access revoked"));

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
        .bridge_reply(Reply::status(StatusCode::TOO_MANY_REQUESTS, "slow down"));

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
        .bridge_reply(Reply::status(StatusCode::PAYMENT_REQUIRED, ""));

    let run = world.sync(connection).await;

    assert_eq!(
        run.detail.as_deref(),
        Some("SimpleFin subscription inactive")
    );
}

/// A pending record the bridge gives no date for is dated the day it is
/// first seen and keeps that day: a later sync does not rewrite it.
#[tokio::test]
async fn undated_pending_record_keeps_its_first_day() {
    let world = world().await;
    let connection = world.seeded().await;
    let undated = account_set(&json!([{
        "id": "TRN-0001", "posted": 0, "amount": "-5.00", "description": "PARKING METER",
    }]));
    world.script.bridge_reply(Reply::ok(&undated));
    world.sync(connection).await;
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    let first_day = world.transactions(account).await[0].posted_on;

    world
        .server
        .clock
        .advance(jiff::SignedDuration::from_hours(48));
    world.script.bridge_reply(Reply::ok(&undated));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded
    );

    let transactions = world.transactions(account).await;
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].posted_on, first_day);
    let updates = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items
        .iter()
        .filter(|event| event.action == "transaction.imported_update")
        .count();
    assert_eq!(updates, 0, "nothing changed, so nothing is audited");
}

#[tokio::test]
async fn amounts_parsed_exactly() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
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
async fn numeric_amount_is_skipped() {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": -12.5,
        "description": "FLOAT AMOUNT",
    }]))));

    let run = world.sync(connection).await;

    // A JSON number is never read as an amount: the record is skipped.
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(run.detail.as_deref().unwrap().contains("1 skipped"));
    let held = world.provider_account("ACT-0001").await;
    assert_eq!(held.error, None);
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(tx.held_sources(held.id).await.unwrap().len(), 0);
    drop(tx);
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
