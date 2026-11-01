//! Which transactions may be created, deleted or patched, and what is audited.

use super::{
    advance, agent, category, imported_transaction, manual_account, manual_transaction, member,
    splits, status, transaction_audit, world,
};
use axum::http::StatusCode;
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::transactions::{ReviewStatus, TransactionOrigin};
use coffer_server::ledger::LedgerError;
use coffer_server::ledger::accounts::AccountPatch;
use coffer_server::ledger::transactions::{NewManualTransactionInput, Patch, TransactionPatch};
use jiff::civil::date;

#[tokio::test]
async fn manual_transaction_refused_on_closed_account() {
    let world = world().await;
    let account = manual_account(&world).await;
    world
        .ledger
        .update_account(
            &world.alice,
            account,
            AccountPatch {
                closed: Some(true),
                ..AccountPatch::default()
            },
        )
        .await
        .unwrap();
    let input = NewManualTransactionInput {
        account,
        posted_on: date(2026, 10, 2),
        minor_units: -100,
        description: "Coffee".into(),
        payee: None,
        memo: None,
        notes: None,
    };
    let err = world
        .ledger
        .create_manual_transaction(&world.alice, input.clone(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Conflict(..)), "{err:?}");
    assert_eq!(status(err), StatusCode::CONFLICT);

    let err = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: AccountId::new(),
                ..input.clone()
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::NOT_FOUND);

    let reader = agent(&world, Scope::Read).await;
    let err = world
        .ledger
        .create_manual_transaction(&reader, input.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);

    // Once reopened it takes transactions again, in its currency.
    world
        .ledger
        .update_account(
            &world.alice,
            account,
            AccountPatch {
                closed: Some(false),
                ..AccountPatch::default()
            },
        )
        .await
        .unwrap();
    let created = world
        .ledger
        .create_manual_transaction(&world.alice, input, None)
        .await
        .unwrap();
    assert_eq!(created.amount.minor_units, -100);
    assert_eq!(created.amount.currency.as_str(), "USD");
    assert_eq!(created.origin, TransactionOrigin::Manual);
    assert!(!created.pending);
    let events = transaction_audit(&world, created.id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "transaction.create");
}

#[tokio::test]
async fn delete_imported_refused() {
    let world = world().await;
    let account = manual_account(&world).await;
    let imported = imported_transaction(&world, account, -400).await;
    let err = world
        .ledger
        .delete_transaction(&world.alice, imported)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Conflict(..)), "{err:?}");
    assert_eq!(status(err), StatusCode::CONFLICT);
    assert!(world.server.store.get_transaction(imported).await.is_ok());

    let manual = manual_transaction(&world, account, -400).await;
    let reader = agent(&world, Scope::Read).await;
    let err = world
        .ledger
        .delete_transaction(&reader, manual.id)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);
    world
        .ledger
        .delete_transaction(&world.alice, manual.id)
        .await
        .unwrap();
    let err = world
        .ledger
        .get_transaction(&world.alice, manual.id)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::NotFound));
    let actions: Vec<_> = transaction_audit(&world, manual.id)
        .await
        .into_iter()
        .map(|event| event.action)
        .collect();
    assert!(
        actions.contains(&"transaction.delete".to_owned()),
        "the audit log lacks the delete action"
    );
    let err = world
        .ledger
        .delete_transaction(&world.alice, manual.id)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn no_op_patch_not_audited() {
    let world = world().await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let bob = member(&world, "bob").await;
    let created = manual_transaction(&world, account, -700).await;
    let full = TransactionPatch {
        payee: Patch::Set("Grocer".into()),
        notes: Patch::Set("weekly".into()),
        review: Some(ReviewStatus::Reviewed),
        assignee: Patch::Set(bob),
        splits: Some(splits(&[(food, -700)])),
    };
    let first = world
        .ledger
        .update_transaction(&world.alice, created.id, full.clone())
        .await
        .unwrap();
    let audited = transaction_audit(&world, created.id).await.len();
    advance(&world);

    for patch in [full, TransactionPatch::default()] {
        let again = world
            .ledger
            .update_transaction(&world.alice, created.id, patch)
            .await
            .unwrap();
        assert_eq!(again, first, "nothing changed, not even updated_at");
    }
    assert_eq!(transaction_audit(&world, created.id).await.len(), audited);

    // Clearing what is already clear changes nothing either.
    let id = imported_transaction(&world, account, -50).await;
    let before = transaction_audit(&world, id).await.len();
    world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Clear,
                notes: Patch::Clear,
                assignee: Patch::Clear,
                review: Some(ReviewStatus::Unreviewed),
                splits: Some(Vec::new()),
            },
        )
        .await
        .unwrap();
    assert_eq!(transaction_audit(&world, id).await.len(), before);
}
