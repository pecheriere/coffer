//! Payee, notes and assignee edits.

use super::{imported_transaction, manual_account, manual_transaction, member, status, world};
use axum::http::StatusCode;
use coffer_domain::ids::MemberId;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{TransactionOrigin, UserState};
use coffer_server::ledger::LedgerError;
use coffer_server::ledger::transactions::{Patch, TransactionPatch};
use jiff::civil::date;

#[tokio::test]
async fn payee_clear_restores_imported() {
    let world = world().await;
    let account = manual_account(&world).await;
    let id = imported_transaction(&world, account, -1_299).await;
    let seeded = world
        .ledger
        .get_transaction(&world.alice, id)
        .await
        .unwrap();
    assert_eq!(seeded.payee.as_deref(), Some("Corner shop"));
    assert_eq!(seeded.description, "CORNER SHOP 0042");
    assert_eq!(seeded.origin, TransactionOrigin::Imported);
    assert_eq!(seeded.transacted_on, Some(date(2026, 10, 2)));

    let edited = world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Set("  Grocer ".into()),
                notes: Patch::Set("milk\nbread".into()),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.payee.as_deref(), Some("Grocer"));
    assert_eq!(edited.notes.as_deref(), Some("milk\nbread"));
    assert!(
        world
            .server
            .store
            .get_transaction(id)
            .await
            .unwrap()
            .payee_edited
    );

    let restored = world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Clear,
                notes: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(restored.payee.as_deref(), Some("Corner shop"));
    assert_eq!(restored.notes, None);
    let stored = world.server.store.get_transaction(id).await.unwrap();
    assert!(!stored.payee_edited);
    assert_eq!(stored.user_state(), UserState::default());

    for (patch, why) in [
        (
            TransactionPatch {
                payee: Patch::Set("   ".into()),
                ..TransactionPatch::default()
            },
            "a blank payee",
        ),
        (
            TransactionPatch {
                payee: Patch::Set("x".repeat(201)),
                ..TransactionPatch::default()
            },
            "a payee over 200 characters",
        ),
        (
            TransactionPatch {
                notes: Patch::Set("tab\there".into()),
                ..TransactionPatch::default()
            },
            "a control character in notes",
        ),
    ] {
        let err = world
            .ledger
            .update_transaction(&world.alice, id, patch)
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{why}: {err:?}");
    }

    // A manual transaction has no imported payee: clearing leaves none.
    let manual = manual_transaction(&world, account, -100).await;
    assert_eq!(manual.payee.as_deref(), Some("Market stall"));
    let cleared = world
        .ledger
        .update_transaction(
            &world.alice,
            manual.id,
            TransactionPatch {
                payee: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.payee, None);
}

#[tokio::test]
async fn assignee_must_be_active_member() {
    let world = world().await;
    let account = manual_account(&world).await;
    let created = manual_transaction(&world, account, -800).await;
    let bob = member(&world, "bob").await;
    let carol = member(&world, "carol").await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_member_disabled(carol, world.server.clock.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    for candidate in [carol, MemberId::new()] {
        let err = world
            .ledger
            .update_transaction(
                &world.alice,
                created.id,
                TransactionPatch {
                    assignee: Patch::Set(candidate),
                    ..TransactionPatch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");
        assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let assigned = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                assignee: Patch::Set(bob),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(assigned.assignee, Some(bob));
    let unassigned = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                assignee: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unassigned.assignee, None);
}
