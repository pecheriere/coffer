//! Splits, categories, review and who recorded them.

use super::{
    advance, agent, category, manual_account, manual_transaction, splits, status,
    transaction_audit, world,
};
use axum::http::StatusCode;
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Scope, Via};
use coffer_domain::ids::CategoryId;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::ReviewStatus;
use coffer_server::ledger::LedgerError;
use coffer_server::ledger::categories::{CategoryGroupPatch, CategoryPatch};
use coffer_server::ledger::transactions::{AttributionView, TransactionPatch};

#[tokio::test]
async fn categorize_records_attribution_with_agent() {
    let world = world().await;
    let claude = agent(&world, Scope::Write).await;
    let Via::Agent { token, .. } = claude.via else {
        unreachable!("an agent actor")
    };
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    let created = manual_transaction(&world, account, -1_000).await;
    assert!(created.splits.is_empty(), "uncategorized");
    assert_eq!(created.categorized_by, None);
    advance(&world);

    let view = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -600), (home, -400)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    let parts: Vec<_> = view
        .splits
        .iter()
        .map(|split| (split.category, split.amount.minor_units))
        .collect();
    assert_eq!(parts, [(food, -600), (home, -400)]);
    assert!(
        view.splits
            .iter()
            .all(|split| split.amount.currency.as_str() == "USD")
    );
    let now = world.server.clock.now();
    assert_eq!(
        view.categorized_by,
        Some(AttributionView {
            member: Some(world.alice.member),
            agent: Some(token),
            system: None,
            at: now,
        })
    );
    assert_eq!(view.reviewed_by, None, "categorizing is not reviewing");
    assert_eq!(view.updated_at, now);
    assert_eq!(
        world
            .ledger
            .get_transaction(&claude, created.id)
            .await
            .unwrap(),
        view
    );

    let events = transaction_audit(&world, created.id).await;
    let update = events
        .iter()
        .find(|event| event.action == "transaction.update")
        .unwrap();
    assert_eq!(
        update.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(token),
        }
    );
    let fields: Vec<_> = update.changes.iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["splits"], "only the changed field");
    let change = update.changes.iter().next().unwrap().1;
    assert_eq!(change.before, AuditValue::from("[]"));
    assert_eq!(
        change.after,
        AuditValue::from(format!(
            "[{{\"category\":\"{food}\",\"minor_units\":-600}},\
             {{\"category\":\"{home}\",\"minor_units\":-400}}]"
        ))
    );
}

#[tokio::test]
async fn splits_are_validated_and_need_write() {
    let world = world().await;
    let claude = agent(&world, Scope::Write).await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    let created = manual_transaction(&world, account, -1_000).await;
    world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -1_000)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    for (bad, why) in [
        (
            splits(&[(food, -600), (home, -399)]),
            "the sum is off by one",
        ),
        (splits(&[(food, -1_000), (home, 0)]), "a zero split"),
        (
            splits(&[(CategoryId::new(), -1_000)]),
            "an unknown category",
        ),
    ] {
        let err = world
            .ledger
            .update_transaction(
                &claude,
                created.id,
                TransactionPatch {
                    splits: Some(bad),
                    ..TransactionPatch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{why}: {err:?}");
        assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }

    // Uncategorizing is a categorization too.
    let cleared = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(Vec::new()),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.splits, []);
    assert_eq!(
        cleared.categorized_by.map(|by| (by.member, by.agent)),
        Some((Some(world.alice.member), None))
    );

    // A read token sees transactions but never changes them.
    let reader = agent(&world, Scope::Read).await;
    assert!(
        world
            .ledger
            .get_transaction(&reader, created.id)
            .await
            .is_ok()
    );
    let err = world
        .ledger
        .update_transaction(
            &reader,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -1_000)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn review_records_reviewer() {
    let world = world().await;
    let account = manual_account(&world).await;
    let created = manual_transaction(&world, account, -250).await;
    assert_eq!(created.review, ReviewStatus::Unreviewed);
    advance(&world);

    let reviewed = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    let reviewed_at = world.server.clock.now();
    assert_eq!(reviewed.review, ReviewStatus::Reviewed);
    assert_eq!(
        reviewed.reviewed_by,
        Some(AttributionView {
            member: Some(world.alice.member),
            agent: None,
            system: None,
            at: reviewed_at,
        })
    );
    assert_eq!(reviewed.categorized_by, None);

    // Reviewing again keeps the first reviewer.
    advance(&world);
    let claude = agent(&world, Scope::Write).await;
    let again = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(again.reviewed_by.map(|by| by.at), Some(reviewed_at));

    let unreviewed = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Unreviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unreviewed.review, ReviewStatus::Unreviewed);
    assert_eq!(unreviewed.reviewed_by, None);

    let events = transaction_audit(&world, created.id).await;
    let reviews: Vec<_> = events
        .iter()
        .filter(|event| event.action == "transaction.update")
        .map(|event| {
            let (field, change) = event.changes.iter().next().unwrap();
            (
                field.to_owned(),
                change.before.clone(),
                change.after.clone(),
            )
        })
        .collect();
    assert_eq!(reviews.len(), 2, "the repeated review wrote nothing");
    assert!(reviews.contains(&(
        "review".into(),
        AuditValue::from("unreviewed"),
        AuditValue::from("reviewed")
    )));
    assert!(reviews.contains(&(
        "review".into(),
        AuditValue::from("reviewed"),
        AuditValue::from("unreviewed")
    )));
}

#[tokio::test]
async fn archived_category_rejected() {
    let world = world().await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let travel = category(&world, "Travel").await;
    let created = manual_transaction(&world, account, -500).await;
    let categorized = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -500)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    let archived = world
        .ledger
        .update_category(
            &world.alice,
            food,
            CategoryPatch {
                archived: Some(true),
                ..CategoryPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(archived.archived);
    let err = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -200), (travel, -300)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");
    assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        world
            .ledger
            .get_transaction(&world.alice, created.id)
            .await
            .unwrap(),
        categorized,
        "unchanged"
    );

    // A category in an archived group is archived too.
    let tree = world.ledger.list_categories(&world.alice).await.unwrap();
    let travel_group = tree
        .items
        .iter()
        .find(|group| group.categories.iter().any(|c| c.id == travel))
        .unwrap()
        .id;
    world
        .ledger
        .update_category_group(
            &world.alice,
            travel_group,
            CategoryGroupPatch {
                archived: Some(true),
                ..CategoryGroupPatch::default()
            },
        )
        .await
        .unwrap();
    let err = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(travel, -500)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");

    // Splits already on an archived category stay, and a patch that
    // repeats them changes nothing.
    let same = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(same.splits, categorized.splits);
}
