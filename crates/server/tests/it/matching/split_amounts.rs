//! Splits against an amount that changes on re-import.

use super::{
    account, advance, apply, bound_link, categorize, category, only_transaction, record, splits,
    transaction_audit, window, world,
};
use coffer_domain::audit::AuditValue;
use coffer_domain::transactions::TransactionFlag;
use coffer_server::ledger::transactions::TransactionPatch;

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
