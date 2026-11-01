//! Un-merging a source record into a transaction of its own.

use super::{
    account, apply, by_write_token, category, get, holding, last_event, link, one_each, patch,
    record, transaction_audit, transactions, unmerge, world,
};
use axum::http::StatusCode;
use coffer_domain::ids::TransactionId;
use coffer_domain::transactions::SplitInput;
use coffer_server::ledger::transactions::{Patch, TransactionPatch};
use jiff::civil::date;

#[tokio::test]
async fn unmerge_restores_separate_transaction() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    let shared = holding(&world, &first, "p-1").await;
    assert_eq!(shared.sources.len(), 2);
    let tools = category(&world, "Tools").await;
    patch(
        &world,
        shared.id,
        TransactionPatch {
            notes: Patch::Set("drill".into()),
            splits: Some(vec![SplitInput {
                category: tools,
                minor_units: -2_999,
            }]),
            review: Some(coffer_domain::transactions::ReviewStatus::Reviewed),
            ..TransactionPatch::default()
        },
    )
    .await;
    let source = shared
        .sources
        .iter()
        .find(|source| source.connection == second.connection)
        .unwrap()
        .id;

    let (status, body) = unmerge(&world, shared.id, source, None).await;
    assert_eq!(status, StatusCode::CREATED);
    let created: TransactionId = body["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(created, shared.id);
    let split_off = get(&world, created).await;
    assert_eq!(split_off.sources.len(), 1);
    assert_eq!(split_off.sources[0].id, source);
    assert_eq!(split_off.amount_minor, -2_999);
    assert!(
        split_off.user_state().is_empty(),
        "the split-off transaction must carry no user state"
    );
    assert_eq!(split_off.flags, Vec::new());
    let kept = get(&world, shared.id).await;
    assert_eq!(kept.sources.len(), 1);
    assert_eq!(kept.sources[0].connection, first.connection);
    assert_eq!(kept.notes.unwrap().as_str(), "drill");
    assert_eq!(kept.splits.len(), 1);
    by_write_token(
        &world,
        &last_event(&world, shared.id, "transaction.unmerge").await,
    );
    by_write_token(
        &world,
        &last_event(&world, created, "transaction.unmerged").await,
    );

    // Later syncs keep them apart.
    let audit_count = transaction_audit(&world).await.len();
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn unmerge_recomputes_the_kept_transactions_fields() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    // A pending charge, then its posted form with a tip, attached.
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -4_000, "BISTRO", true)],
    )
    .await;
    let mut posted = record("posted-1", 11, -4_600, "BISTRO", false);
    posted.replaces_provider_id = Some("pending-1".into());
    apply(&world, &link, vec![posted]).await;
    let joined = holding(&world, &link, "pending-1").await;
    assert_eq!(joined.amount_minor, -4_600);
    let dining = category(&world, "Dining").await;
    patch(
        &world,
        joined.id,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: dining,
                minor_units: -4_600,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    let posted_source = joined
        .sources
        .iter()
        .find(|s| !s.fields.pending)
        .unwrap()
        .id;

    let (status, body) = unmerge(&world, joined.id, posted_source, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["amount"]["minor_units"], -4_600);
    assert_eq!(body["pending"], false);
    let kept = get(&world, joined.id).await;
    assert!(kept.pending, "only its pending record is left");
    assert_eq!(kept.amount_minor, -4_000);
    assert_eq!(kept.posted_on, date(2026, 10, 10));
    assert_eq!(kept.splits[0].minor_units, -4_000, "a single split follows");
}

#[tokio::test]
async fn unmerge_with_idempotency_key_replays() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase()).await;
    apply(&world, &second, purchase()).await;
    let shared = holding(&world, &first, "p-1").await;
    let source = shared.sources[1].id;

    let (status, body) = unmerge(&world, shared.id, source, Some("split-1")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (again, replayed) = unmerge(&world, shared.id, source, Some("split-1")).await;
    assert_eq!(again, StatusCode::CREATED);
    assert_eq!(replayed["id"], body["id"]);
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
}

#[tokio::test]
async fn unmerge_last_source_refused() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let ids = one_each(&world, &[&link], &[-500]).await;
    apply(&world, &link, vec![record("p-2", 11, -700, "OTHER", false)]).await;
    let other = holding(&world, &link, "p-2").await;
    let only = get(&world, ids[0]).await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = unmerge(&world, only.id, only.sources[0].id, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");
    // A record of another transaction is not one of its own.
    let (status, body) = unmerge(&world, only.id, other.sources[0].id, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!body.to_string().contains(&other.sources[0].id.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}
