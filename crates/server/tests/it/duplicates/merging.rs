//! Merging two imported transactions, and who may.

use super::{
    account, apply, before, by_write_token, category, flag_with, flagged_pair, get, holding,
    last_event, link, merge, one_each, patch, record, send, transaction_audit, transactions, world,
};
use axum::http::StatusCode;
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::TransactionId;
use coffer_domain::transactions::{SplitInput, TransactionFlag};
use coffer_server::ledger::transactions::{NewManualTransactionInput, Patch, TransactionPatch};
use jiff::civil::date;
use serde_json::json;

#[tokio::test]
async fn merge_moves_sources_and_refuses_user_state() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let groceries = category(&world, "Groceries").await;
    // The target's user state, which the merge keeps.
    patch(
        &world,
        posted,
        TransactionPatch {
            notes: Patch::Set("weekly shop".into()),
            splits: Some(vec![SplitInput {
                category: groceries,
                minor_units: -1_250,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    // The duplicate's, which a merge refuses to drop unless told to.
    patch(
        &world,
        pending,
        TransactionPatch {
            notes: Patch::Set("from the receipt".into()),
            ..TransactionPatch::default()
        },
    )
    .await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, posted, json!({ "duplicate": pending })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "merge_conflict");
    assert!(!body.to_string().contains(&pending.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);

    let (status, body) = merge(
        &world,
        posted,
        json!({ "duplicate": pending, "discard_duplicate_state": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], posted.to_string());
    let merged = get(&world, posted).await;
    assert_eq!(
        transactions(&world, checking.id).await,
        std::slice::from_ref(&merged)
    );
    let mut providers: Vec<&str> = merged
        .sources
        .iter()
        .map(|source| source.provider_txn_id.as_str())
        .collect();
    providers.sort_unstable();
    assert_eq!(providers, ["pending-1", "posted-1"]);
    assert_eq!(merged.notes.unwrap().as_str(), "weekly shop");
    assert_eq!(merged.splits.len(), 1);
    assert!(!merged.pending, "the posted target keeps its fields");
    assert_eq!(merged.flags, Vec::new());
    assert_eq!(merged.duplicate_candidates, Vec::new());

    let discarded = last_event(&world, pending, "transaction.merged").await;
    by_write_token(&world, &discarded);
    assert_eq!(
        before(&discarded, "notes"),
        AuditValue::Text("from the receipt".into())
    );
    let event = last_event(&world, posted, "transaction.merge").await;
    by_write_token(&world, &event);
    assert_eq!(before(&event, "possible_duplicate"), AuditValue::Bool(true));

    // The merged records stay on the target through later syncs.
    let audit_count = transaction_audit(&world).await.len();
    apply(
        &world,
        &link,
        vec![
            record("pending-1", 10, -1_250, "CORNER SHOP", true),
            record("posted-1", 11, -1_250, "CORNER SHOP 0042", false),
        ],
    )
    .await;
    assert_eq!(transactions(&world, checking.id).await.len(), 1);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merge_takes_the_posted_fields_of_a_posted_duplicate() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let groceries = category(&world, "Groceries").await;
    patch(
        &world,
        pending,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: groceries,
                minor_units: -1_250,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;

    // The pending transaction holds the user state: it is the target.
    let (status, _) = merge(&world, pending, json!({ "duplicate": posted })).await;
    assert_eq!(status, StatusCode::OK);
    let merged = get(&world, pending).await;
    assert!(!merged.pending);
    assert_eq!(merged.posted_on, date(2026, 10, 11));
    assert_eq!(merged.raw_description.as_str(), "CORNER SHOP 0042");
    assert_eq!(merged.splits.len(), 1, "the target's splits stay");
    assert_eq!(merged.sources.len(), 2);
    assert_eq!(merged.flags, Vec::new());
}

#[tokio::test]
async fn merge_across_accounts_refused() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let savings = account(&world, "Savings").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &savings).await;
    let ids = one_each(&world, &[&first, &second], &[-500, -500]).await;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, ids[0], json!({ "duplicate": ids[1] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid");
    assert_eq!(get(&world, ids[1]).await.sources.len(), 1);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merge_refuses_itself_manual_and_unknown_transactions() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let ids = one_each(&world, &[&link], &[-500]).await;
    let cash = account(&world, "Cash").await;
    let manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: cash.id,
                posted_on: date(2026, 10, 10),
                minor_units: -500,
                description: "Market".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();
    let other_manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: cash.id,
                posted_on: date(2026, 10, 10),
                minor_units: -500,
                description: "Market".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();

    let (status, _) = merge(&world, ids[0], json!({ "duplicate": ids[0] })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = merge(&world, manual.id, json!({ "duplicate": other_manual.id })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = TransactionId::new();
    let (status, body) = merge(&world, ids[0], json!({ "duplicate": unknown })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_string().contains(&unknown.to_string()));
    let (status, _) = merge(&world, unknown, json!({ "duplicate": ids[0] })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn merge_clears_flag_when_last_candidate_goes() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let links = [
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
    ];
    let ids = one_each(&world, &[&links[0], &links[1], &links[2]], &[-1, -2, -3]).await;
    let (target, first, second) = (ids[0], ids[1], ids[2]);
    flag_with(&world, target, &[first, second]).await;

    let (status, _) = merge(&world, target, json!({ "duplicate": first })).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, target).await;
    assert_eq!(after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(after.duplicate_candidates, [second]);

    let (status, body) = merge(&world, target, json!({ "duplicate": second })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["flags"], json!([]));
    let after = get(&world, target).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(after.sources.len(), 3);
    let event = last_event(&world, target, "transaction.merge").await;
    assert_eq!(before(&event, "possible_duplicate"), AuditValue::Bool(true));
}

#[tokio::test]
async fn merge_carries_the_duplicates_candidacies_to_the_target() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let links = [
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
        link(&world, &checking).await,
    ];
    let fifth = link(&world, &checking).await;
    let ids = one_each(
        &world,
        &[&links[0], &links[1], &links[2], &links[3], &fifth],
        &[-1, -2, -3, -4, -5],
    )
    .await;
    let (target, duplicate, candidate, lister) = (ids[0], ids[1], ids[2], ids[3]);
    let both = ids[4];
    // The duplicate may itself duplicate `candidate`, and `lister` may
    // duplicate it: both now concern the target. `both` already listed the
    // target: it only loses the duplicate.
    flag_with(&world, duplicate, &[target, candidate]).await;
    flag_with(&world, lister, &[duplicate]).await;
    flag_with(&world, both, &[duplicate, target]).await;

    let (status, _) = merge(&world, target, json!({ "duplicate": duplicate })).await;
    assert_eq!(status, StatusCode::OK);
    let after = get(&world, target).await;
    assert_eq!(after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(after.duplicate_candidates, [candidate]);
    let lister_after = get(&world, lister).await;
    assert_eq!(lister_after.flags, [TransactionFlag::PossibleDuplicate]);
    assert_eq!(lister_after.duplicate_candidates, [target]);
    by_write_token(
        &world,
        &last_event(&world, lister, "transaction.candidate_replaced").await,
    );
    assert_eq!(get(&world, both).await.duplicate_candidates, [target]);
    let removed = last_event(&world, both, "transaction.candidate_removed").await;
    by_write_token(&world, &removed);
    assert_eq!(
        before(&removed, "duplicate_candidate"),
        AuditValue::Text(duplicate.to_string())
    );
    assert!(
        !transaction_audit(&world).await.iter().any(|event| {
            event.entity_id == both.to_string() && event.action == "transaction.candidate_replaced"
        }),
        "nothing was replaced"
    );
}

#[tokio::test]
async fn merge_drops_candidates_the_same_provider_reported_apart() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    // Two purchases the second provider reports apart, and one from the
    // first provider that matches both: flagged as ambiguous.
    apply(
        &world,
        &second,
        vec![
            record("b-1", 10, -2_999, "HARDWARE STORE", false),
            record("b-2", 10, -2_999, "HARDWARE STORE", false),
        ],
    )
    .await;
    apply(
        &world,
        &first,
        vec![record("a-1", 10, -2_999, "HARDWARE STORE", false)],
    )
    .await;
    let flagged = holding(&world, &first, "a-1").await;
    let one = holding(&world, &second, "b-1").await.id;
    let two = holding(&world, &second, "b-2").await.id;
    assert_eq!(flagged.duplicate_candidates.len(), 2);

    let (status, _) = merge(&world, flagged.id, json!({ "duplicate": one })).await;
    assert_eq!(status, StatusCode::OK);
    // The target now holds b-1, which the second provider reported apart
    // from b-2: it cannot also be b-2's purchase.
    let after = get(&world, flagged.id).await;
    assert_eq!(after.duplicate_candidates, Vec::new());
    assert_eq!(after.flags, Vec::new());
    let event = last_event(&world, flagged.id, "transaction.candidate_removed").await;
    by_write_token(&world, &event);
    assert_eq!(
        before(&event, "duplicate_candidate"),
        AuditValue::Text(two.to_string())
    );
}

#[tokio::test]
async fn candidate_turning_posted_clears_stale_flag() {
    let world = world().await;
    // The pending candidate attaches its posted form (a record naming the
    // one it replaces): the flagged posted purchase is another one.
    let checking = account(&world, "Checking").await;
    let attaching = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &attaching).await;
    let mut replacement = record("final-1", 11, -1_250, "CORNER SHOP", false);
    replacement.replaces_provider_id = Some("pending-1".into());
    apply(&world, &attaching, vec![replacement]).await;
    assert_eq!(get(&world, pending).await.sources.len(), 2);
    let after = get(&world, posted).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
    let event = last_event(&world, posted, "transaction.candidate_removed").await;
    assert_eq!(
        event.actor,
        AuditActor::System {
            reason: "system:sync".into()
        }
    );

    // The pending record itself is re-reported as posted.
    let savings = account(&world, "Savings").await;
    let in_place = link(&world, &savings).await;
    let (_, posted) = flagged_pair(&world, &in_place).await;
    apply(
        &world,
        &in_place,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", false)],
    )
    .await;
    let after = get(&world, posted).await;
    assert_eq!(after.flags, Vec::new());
    assert_eq!(after.duplicate_candidates, Vec::new());
}

#[tokio::test]
async fn read_token_cannot_merge() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let (_, read) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let source = get(&world, posted).await.sources[0].id;
    let audit_count = transaction_audit(&world).await.len();

    for (uri, body) in [
        (
            format!("/api/v1/transactions/{posted}/merge"),
            Some(json!({ "duplicate": pending })),
        ),
        (
            format!("/api/v1/transactions/{posted}/merge"),
            Some(json!({ "duplicate": posted })),
        ),
        (
            format!("/api/v1/transactions/{posted}/unmerge"),
            Some(json!({ "source": source })),
        ),
        (
            format!("/api/v1/transactions/{posted}/dismiss-duplicate"),
            None,
        ),
    ] {
        let (status, problem) = send(&world, &read, &uri, body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(problem["code"], "forbidden");
    }
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

#[tokio::test]
async fn merged_records_of_two_connections_stay_put_through_resyncs() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let first = link(&world, &checking).await;
    let second = link(&world, &checking).await;
    // The same purchase, reported differently enough not to match.
    let one = || vec![record("a-1", 10, -2_999, "HARDWARE STORE", false)];
    let other = || vec![record("b-1", 13, -3_010, "HDW STORE 12 CITY", false)];
    apply(&world, &first, one()).await;
    apply(&world, &second, other()).await;
    let target = holding(&world, &first, "a-1").await.id;
    let duplicate = holding(&world, &second, "b-1").await.id;
    let tools = category(&world, "Tools").await;
    patch(
        &world,
        target,
        TransactionPatch {
            splits: Some(vec![SplitInput {
                category: tools,
                minor_units: -2_999,
            }]),
            ..TransactionPatch::default()
        },
    )
    .await;
    let (status, _) = merge(&world, target, json!({ "duplicate": duplicate })).await;
    assert_eq!(status, StatusCode::OK);
    let merged = get(&world, target).await;
    let audit_count = transaction_audit(&world).await.len();

    for _ in 0..2 {
        world
            .server
            .clock
            .advance(jiff::SignedDuration::from_secs(60));
        apply(&world, &first, one()).await;
        apply(&world, &second, other()).await;
    }
    let after = get(&world, target).await;
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
    assert_eq!(after.amount_minor, merged.amount_minor);
    assert_eq!(after.posted_on, merged.posted_on);
    assert_eq!(after.raw_description, merged.raw_description);
    assert_eq!(after.splits, merged.splits);
    assert_eq!(after.updated_at, merged.updated_at);
}

#[tokio::test]
async fn merge_refuses_purchases_one_connection_reported_apart() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![
            record("p-1", 10, -500, "MARKET", false),
            record("p-2", 10, -700, "MARKET", false),
        ],
    )
    .await;
    let one = holding(&world, &link, "p-1").await.id;
    let two = holding(&world, &link, "p-2").await.id;
    let audit_count = transaction_audit(&world).await.len();

    let (status, body) = merge(&world, one, json!({ "duplicate": two })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid");
    assert!(!body.to_string().contains(&two.to_string()));
    assert_eq!(transactions(&world, checking.id).await.len(), 2);
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}
