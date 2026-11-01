//! Dismissing a duplicate flag.

use super::{
    account, apply, before, by_write_token, dismiss, flag_with, flagged_pair, get, last_event,
    link, one_each, record, transaction_audit, transactions, world,
};
use axum::http::StatusCode;
use coffer_domain::audit::AuditValue;
use coffer_domain::transactions::TransactionFlag;
use serde_json::json;

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
