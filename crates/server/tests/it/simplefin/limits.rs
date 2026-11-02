//! What one account set may hold: the accounts it lists, the transactions of
//! each, and the transactions of all. An answer over a limit fails the run
//! with a fixed text and leaves what is stored as it was.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{Reply, World, account_set, world};
use coffer_domain::accounts::AccountKind;
use coffer_domain::sync::SyncRunStatus;
use serde_json::json;

const TOO_MANY_ACCOUNTS: &str = "SimpleFin listed more accounts than one sync accepts";
const TOO_MANY_TRANSACTIONS: &str = "SimpleFin sent more transactions than one sync accepts";
const UNREADABLE: &str = "SimpleFin sent an answer coffer could not read";
const MAX_ACCOUNTS: usize = 1_000;
const MAX_ACCOUNT_TRANSACTIONS: usize = 10_000;
const MAX_ANSWER_TRANSACTIONS: usize = 100_000;

/// `count` entries too small to read, each costing a few bytes.
fn empties(count: usize) -> String {
    vec!["{}"; count].join(",")
}

/// One account whose transaction list is `transactions`, written as JSON.
fn account_with(id: &str, transactions: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"A","currency":"USD","balance":"1.00","balance-date":1799992800,"transactions":[{transactions}]}}"#
    )
}

fn answer(accounts: &[String]) -> String {
    format!(r#"{{"errors":[],"accounts":[{}]}}"#, accounts.join(","))
}

/// A world that has synced one bound account holding one transaction.
async fn synced() -> (
    World,
    coffer_domain::ids::ConnectionId,
    coffer_domain::ids::AccountId,
) {
    let world = world().await;
    let connection = world.seeded().await;
    world.script.bridge_reply(Reply::ok(&account_set(&json!([{
        "id": "TRN-0001", "posted": 1_799_745_300, "amount": "-5.00", "description": "COFFEE",
    }]))));
    assert_eq!(
        world.sync(connection).await.status,
        SyncRunStatus::Succeeded,
        "the first sync works"
    );
    let account = world.bind("ACT-0001", AccountKind::Checking).await;
    (world, connection, account)
}

/// Syncs `body` over the stored data of [`synced`] and checks the run failed
/// with `detail` and that nothing stored changed.
async fn refused(body: &str, detail: &str) {
    let (world, connection, account) = synced().await;
    let before = (
        world.transactions(account).await.len(),
        world
            .provider_account("ACT-0001")
            .await
            .last_balance
            .unwrap()
            .amount_raw,
    );
    world.script.bridge_reply(Reply::ok(body));

    let run = world.sync(connection).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some(detail));
    let after = (
        world.transactions(account).await.len(),
        world
            .provider_account("ACT-0001")
            .await
            .last_balance
            .unwrap()
            .amount_raw,
    );
    assert_eq!(after, before, "a refused answer changes nothing stored");
    assert_eq!(
        world.provider_account("ACT-0001").await.error,
        None,
        "and marks nothing stored as failed"
    );
}

/// Syncs `body` and checks the run was read, not refused.
async fn accepted(body: &str) {
    let (world, connection, _) = synced().await;
    world.script.bridge_reply(Reply::ok(body));

    let run = world.sync(connection).await;

    assert_ne!(run.status, SyncRunStatus::Failed, "{:?}", run.detail);
}

#[tokio::test]
async fn an_answer_of_exactly_the_account_limit_is_read() {
    accepted(&format!(r#"{{"accounts":[{}]}}"#, empties(MAX_ACCOUNTS))).await;
}

#[tokio::test]
async fn an_answer_one_account_over_the_limit_is_refused() {
    refused(
        &format!(r#"{{"accounts":[{}]}}"#, empties(MAX_ACCOUNTS + 1)),
        TOO_MANY_ACCOUNTS,
    )
    .await;
}

/// Past the limit the rest of the list is skipped, not trusted: a syntax
/// error there still fails the answer, as an unreadable one.
#[tokio::test]
async fn malformed_json_after_the_account_limit_is_unreadable() {
    refused(
        &format!(
            r#"{{"accounts":[{},{{"id":}}]}}"#,
            empties(MAX_ACCOUNTS + 1)
        ),
        UNREADABLE,
    )
    .await;
}

#[tokio::test]
async fn an_account_of_exactly_the_transaction_limit_is_read() {
    accepted(&answer(&[account_with(
        "big",
        &empties(MAX_ACCOUNT_TRANSACTIONS),
    )]))
    .await;
}

#[tokio::test]
async fn an_account_one_transaction_over_the_limit_is_refused() {
    refused(
        &answer(&[account_with("big", &empties(MAX_ACCOUNT_TRANSACTIONS + 1))]),
        TOO_MANY_TRANSACTIONS,
    )
    .await;
}

#[tokio::test]
async fn malformed_json_after_the_transaction_limit_is_unreadable() {
    refused(
        &answer(&[account_with(
            "big",
            &format!("{},{{\"id\":}}", empties(MAX_ACCOUNT_TRANSACTIONS + 1)),
        )]),
        UNREADABLE,
    )
    .await;
}

/// The transactions of all accounts together may reach the answer limit.
#[tokio::test]
async fn an_answer_of_exactly_the_total_transaction_limit_is_read() {
    let each = empties(MAX_ACCOUNT_TRANSACTIONS);
    let accounts: Vec<String> = (0..MAX_ANSWER_TRANSACTIONS / MAX_ACCOUNT_TRANSACTIONS)
        .map(|n| account_with(&format!("big-{n}"), &each))
        .collect();
    accepted(&answer(&accounts)).await;
}

/// Accounts each within their limit, whose sum is one over the answer limit.
#[tokio::test]
async fn an_answer_one_transaction_over_the_total_limit_is_refused() {
    let each = empties(MAX_ACCOUNT_TRANSACTIONS);
    let mut accounts: Vec<String> = (0..MAX_ANSWER_TRANSACTIONS / MAX_ACCOUNT_TRANSACTIONS)
        .map(|n| account_with(&format!("big-{n}"), &each))
        .collect();
    accounts.push(account_with("last", &empties(1)));
    refused(&answer(&accounts), TOO_MANY_TRANSACTIONS).await;
}

/// A large honest household over the bridge's 90 days: forty accounts of
/// a thousand real records each, 40 percent of the answer limit, is read.
#[tokio::test]
async fn a_large_household_over_ninety_days_is_read() {
    let record = |n: usize| {
        format!(r#"{{"id":"t-{n}","posted":1799745300,"amount":"-1.00","description":"x"}}"#)
    };
    let transactions = (0..1_000).map(record).collect::<Vec<_>>().join(",");
    let accounts: Vec<String> = (0..40)
        .map(|n| account_with(&format!("home-{n}"), &transactions))
        .collect();
    let (world, connection, _) = synced().await;
    world.script.bridge_reply(Reply::ok(&answer(&accounts)));

    let run = world.sync(connection).await;

    // The other accounts are not bound yet, so the run is partial; none of
    // the records is skipped.
    assert_ne!(run.status, SyncRunStatus::Failed, "{:?}", run.detail);
    assert!(
        run.detail.as_deref().unwrap().ends_with("0 skipped"),
        "{:?}",
        run.detail
    );
    let held = world
        .server
        .store
        .list_provider_accounts(None)
        .await
        .unwrap();
    assert_eq!(held.len(), 41, "the 40 accounts and the one synced before");
}
