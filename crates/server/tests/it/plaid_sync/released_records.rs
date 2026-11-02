//! The sync after a release: which records it gives up, and what leaves the
//! hold and the release in place.

use super::{ACCOUNTS, GET_ACCOUNTS, SYNC, World, accounts_answer, page, record, world};
use axum::http::StatusCode;
use coffer_domain::transactions::SplitInput;
use coffer_server::ledger::transactions::TransactionPatch;
use coffer_server::sync::connections::HELD_AFTER_SYNCS;
use coffer_server::testkit::{
    INJECTED_BIND_RECORD_ID, INJECTED_FAILURE_RECORD_ID, INJECTED_FAULT_RECORD_ID,
};
use serde_json::json;

const LINE: &str = "acct-checking";

impl World {
    async fn holds_then_releases(&self) {
        self.syncs(HELD_AFTER_SYNCS).await;
        self.release().await.unwrap();
    }

    async fn release_still_stands(&self) -> bool {
        let line = self.provider_account(LINE).await;
        line.held_syncs >= HELD_AFTER_SYNCS
            && line.hold_released
            && self.connection().await.provider_state.is_none()
            && self.audits("connection.cursor_passed").await.is_empty()
    }
}

/// A busy database says nothing about the record: nothing is given up, and
/// the release applies once the database answers again.
#[tokio::test]
async fn a_database_fault_gives_up_no_record() {
    let world = world().await;
    let checking = world.bound(LINE).await;
    world.server.inject_source_insert_fault().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[
                record(INJECTED_FAULT_RECORD_ID, LINE, "1"),
                record("later", LINE, "2"),
            ],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.holds_then_releases().await;

    let run = world.sync().await;

    assert!(run.detail.unwrap().contains("0 skipped"));
    assert!(world.release_still_stands().await);

    world.server.clear_source_insert_fault().await;
    world.sync().await;

    assert_eq!(world.transactions(checking).await.len(), 2);
    assert_eq!(
        world.connection().await.provider_state.as_deref(),
        Some("cursor-1")
    );
}

/// As when a delta applies at once, a posted record finds the pending one the
/// same delta removes, without `pending_transaction_id`: it joins that
/// transaction, and what the member did to it stays.
#[tokio::test]
async fn a_posted_record_joins_the_pending_one_its_delta_removes() {
    let world = world().await;
    let checking = world.bound(LINE).await;
    let mut pending = record("pending", LINE, "4.5");
    pending["pending"] = json!(true);
    world
        .script
        .page(page(&[pending], &[], &[], "cursor-0", false));
    world.sync().await;
    let before = world.transactions(checking).await.remove(0);
    let food = world.category().await;
    world
        .server
        .app_state()
        .ledger
        .update_transaction(
            &world.alice,
            before.id,
            TransactionPatch {
                splits: Some(vec![SplitInput {
                    category: food,
                    minor_units: before.amount_minor,
                }]),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    world.server.inject_source_insert_failure().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[
                record("posted", LINE, "4.5"),
                record(INJECTED_FAILURE_RECORD_ID, LINE, "9"),
            ],
            &[],
            &[json!({ "transaction_id": "pending", "account_id": LINE })],
            "cursor-1",
            false,
        ),
    );
    world.holds_then_releases().await;

    let run = world.sync().await;

    assert!(run.detail.unwrap().contains("1 skipped"));
    let after = world.transactions(checking).await;
    assert_eq!(after.len(), 1, "no second transaction");
    assert_eq!(after[0].id, before.id);
    assert!(!after[0].pending);
    assert_eq!(after[0].splits[0].category, food);
    assert_eq!(after[0].sources.len(), 1);
    assert_eq!(after[0].sources[0].provider_txn_id, "posted");
}

/// A refusal that differs from what the member saw takes the release back,
/// however long it lasts: nothing is given up until the member asks again.
#[tokio::test]
async fn a_new_refusal_withdraws_the_release_and_gives_up_no_record() {
    let world = world().await;
    let checking = world.bound(LINE).await;
    world.server.inject_source_insert_failure().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[
                record("fine", LINE, "1"),
                record(INJECTED_FAILURE_RECORD_ID, LINE, "2"),
            ],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.holds_then_releases().await;
    let id = world.provider_account(LINE).await.id;
    world
        .script
        .always(GET_ACCOUNTS, StatusCode::OK, accounts_answer(&[]));

    for _ in 0..2 {
        let run = world.sync().await;
        assert!(run.detail.unwrap().contains("0 skipped"));
        assert_eq!(world.connection().await.provider_state, None);
    }

    assert_eq!(world.audits("connection.cursor_passed").await.len(), 0);
    let held = world.held_cursor().await.unwrap();
    assert!(!held.release_pending);
    assert!(held.release_withdrawn);
    let withdrawn = world.audits("connection.release_withdrawn").await;
    assert_eq!(withdrawn.len(), 1, "taken back once, and audited");
    let changes = serde_json::to_string(&withdrawn[0].changes).unwrap();
    assert!(changes.contains(&id.to_string()));
    assert!(changes.contains("differs"), "the entry says why");
    assert!(!changes.contains("did not list"), "no provider text");

    world.script.always(GET_ACCOUNTS, StatusCode::OK, ACCOUNTS);
    world.release().await.unwrap();
    assert!(!world.held_cursor().await.unwrap().release_withdrawn);
    let run = world.sync().await;

    assert!(run.detail.unwrap().contains("1 skipped"));
    assert_eq!(world.transactions(checking).await.len(), 1);
}

/// A binding made while the records apply one by one reaches the later ones:
/// they become transactions, since no later bind or fetch would make them so.
#[tokio::test]
async fn a_binding_made_mid_run_reaches_the_later_records() {
    let world = world().await;
    let target = world.account().await;
    world.server.inject_source_insert_failure().await;
    world.server.inject_bind_on_source_insert().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[
                record(INJECTED_BIND_RECORD_ID, LINE, "1"),
                record("later", LINE, "2"),
                record(INJECTED_FAILURE_RECORD_ID, LINE, "3"),
            ],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.holds_then_releases().await;

    world.sync().await;

    assert_eq!(world.provider_account(LINE).await.account, Some(target));
    let stored = world.transactions(target).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sources[0].provider_txn_id, "later");
}

/// Once a record is given up, the source no longer says it comes back.
#[tokio::test]
async fn a_pass_stops_saying_the_records_are_fetched_again() {
    let world = world().await;
    world.bound(LINE).await;
    world.server.inject_source_insert_failure().await;
    world.script.always(
        SYNC,
        StatusCode::OK,
        page(
            &[record(INJECTED_FAILURE_RECORD_ID, LINE, "1")],
            &[],
            &[],
            "cursor-1",
            false,
        ),
    );
    world.syncs(HELD_AFTER_SYNCS).await;
    let held = world.provider_account(LINE).await.error.unwrap();
    assert!(held.contains("fetches the missing records again"));
    world.release().await.unwrap();

    world.sync().await;

    let passed = world.provider_account(LINE).await.error.unwrap();
    assert!(passed.contains("given up"));
}
