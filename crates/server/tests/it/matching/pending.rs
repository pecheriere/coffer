//! Pending and posted forms of one purchase, attached or flagged without
//! duplicating it.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{
    account, actions_for, advance, apply, bound_link, categorize, category, ignoring_last_seen,
    only_transaction, record, transaction_audit, transactions, window, world,
};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::transactions::TransactionFlag;
use coffer_server::ledger::transactions::{Patch, TransactionPatch};
use coffer_server::sync::apply::{ApplyReport, Coverage};
use coffer_store::TransactionRecord;
use jiff::civil::date;

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
