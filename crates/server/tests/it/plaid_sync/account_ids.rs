//! One rule for the ids of Plaid accounts: an empty id or one with a control
//! character is as unusable as an over-long one, so it cannot hold the
//! cursor for good.

use super::{GET_ACCOUNTS, SYNC, account_json, accounts_answer, page, record, world};
use axum::http::StatusCode;
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::apply::MAX_PROVIDER_ID_CHARS;
use coffer_store::ConnectionStatus;
use serde_json::{Value, json};

const NO_ACCOUNT: &str = "Plaid sent records coffer could not read or assign to an account";
const NO_ACCOUNT_ID: &str = "Plaid sent an account without a usable id";

fn unusable_ids() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("newline", "acct\nx".to_owned()),
        ("escape", "acct\u{1b}x".to_owned()),
        ("over-long", "i".repeat(MAX_PROVIDER_ID_CHARS + 1)),
    ]
}

/// A record of the account `account`, whatever characters its id holds.
fn record_of(id: &str, account: &str) -> Value {
    let mut record = record(id, "acct-checking", "1");
    record["account_id"] = json!(account);
    record
}

#[tokio::test]
async fn records_of_an_unusable_account_id_are_counted_and_never_hold_the_cursor() {
    for (kind, id) in unusable_ids() {
        let world = world().await;
        let checking = world.bound("acct-checking").await;
        world.script.page(page(
            &[
                record("good", "acct-checking", "1"),
                record_of("bad-added", &id),
            ],
            &[record_of("bad-modified", &id)],
            &[json!({ "transaction_id": "gone", "account_id": id })],
            "done",
            false,
        ));

        let run = world.sync().await;

        assert_eq!(run.status, SyncRunStatus::Partial, "{kind}");
        assert!(
            run.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("3 skipped")),
            "{kind}: the three records are counted"
        );
        assert_eq!(world.transactions(checking).await.len(), 1, "{kind}");
        let connection = world.connection().await;
        assert_eq!(connection.provider_state.as_deref(), Some("done"), "{kind}");
        assert_eq!(connection.last_error.as_deref(), Some(NO_ACCOUNT), "{kind}");
        assert_eq!(connection.status, ConnectionStatus::Active, "{kind}");
    }
}

#[tokio::test]
async fn an_unusable_account_id_on_an_account_is_reported_and_the_others_sync() {
    for (kind, id) in unusable_ids() {
        let world = world().await;
        let good = world.bound("good").await;
        world.script.always(
            GET_ACCOUNTS,
            StatusCode::OK,
            accounts_answer(&[
                account_json("good", "depository", &json!(1)),
                account_json(&id, "depository", &json!(1)),
            ]),
        );
        world.script.page(page(
            &[record("kept", "good", "1")],
            &[],
            &[],
            "done",
            false,
        ));

        let run = world.sync().await;

        assert_eq!(run.status, SyncRunStatus::Partial, "{kind}");
        assert_eq!(world.transactions(good).await.len(), 1, "{kind}");
        assert_eq!(
            world.connection().await.last_error.as_deref(),
            Some(NO_ACCOUNT_ID),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn a_record_last_seen_with_an_unusable_account_id_is_not_applied_from_an_earlier_page() {
    let world = world().await;
    let checking = world.bound("acct-checking").await;
    world.script.page(page(
        &[
            record("stale", "acct-checking", "1"),
            record("good", "acct-checking", "1"),
        ],
        &[],
        &[],
        "next",
        true,
    ));
    world.script.reply(
        SYNC,
        StatusCode::OK,
        page(
            &[],
            &[],
            &[json!({ "transaction_id": "stale", "account_id": "" })],
            "done",
            false,
        ),
    );

    let run = world.sync().await;

    assert_eq!(run.status, SyncRunStatus::Partial);
    let stored = world.transactions(checking).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "good");
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("done"),
        "the cursor moves on"
    );
}
