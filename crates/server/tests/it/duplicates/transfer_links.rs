//! A link through a merge, an un-merge and a sync (spec §8.12 T7): only a person ends one, and
//! a provider's record coming and going never does.

use axum::http::StatusCode;
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::TransactionId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::TransactionFlag;
use coffer_server::sync::apply::{AccountBatch, Coverage, apply_account_batch};
use coffer_store::{AuditFilter, PageRequest};
use serde_json::{Value, json};

use super::{
    Link, World, account, apply, flagged_pair, get, holding, link, merge, record, send, unmerge,
    world,
};

const LINKS: &str = "/api/v1/transfer-links";

/// A manual deposit in an account of its own, for the other side of a link.
async fn the_other_side(world: &World, minor_units: i64) -> TransactionId {
    let savings = account(world, "Savings").await;
    let (status, body) = send(
        world,
        &world.write,
        "/api/v1/transactions",
        Some(json!({
            "account": savings.id,
            "posted_on": "2026-10-11",
            "minor_units": minor_units,
            "description": "Moved",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    serde_json::from_value(body["id"].clone()).unwrap()
}

async fn linked(world: &World, outflow: TransactionId, inflow: TransactionId) -> Value {
    let (status, body) = send(
        world,
        &world.write,
        LINKS,
        Some(json!({"outflow": outflow, "inflow": inflow})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body
}

/// A merge into the leg keeps its link: the target keeps what it holds, and it takes the
/// posted fields as it would without one.
#[tokio::test]
async fn a_merge_into_a_leg_keeps_its_link() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let inflow = the_other_side(&world, 1_250).await;
    let made = linked(&world, pending, inflow).await;

    let (status, body) = merge(&world, pending, json!({"duplicate": posted})).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["transfer_link"]["id"], made["id"]);
    assert_eq!(body["pending"], false);
}

/// A link is user state: a merge doesn't drop it unasked, and told to, it records that it did.
#[tokio::test]
async fn a_merge_of_a_leg_is_refused_unless_told_to_discard_it() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let inflow = the_other_side(&world, 1_250).await;
    let made = linked(&world, pending, inflow).await;

    let (status, body) = merge(&world, posted, json!({"duplicate": pending})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "merge_conflict");
    assert!(get(&world, pending).await.transfer_link.is_some());

    let (status, body) = merge(
        &world,
        posted,
        json!({"duplicate": pending, "discard_duplicate_state": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["transfer_link"], Value::Null);
    assert_eq!(get(&world, inflow).await.transfer_link, None);

    let id = made["id"].as_str().unwrap();
    let audit = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items;
    let removal = audit
        .iter()
        .find(|event| event.action == "transfer_link.remove")
        .unwrap();
    assert_eq!(removal.entity_id, id);
}

/// A token that can't write the other side can't end the link, even by a merge it may make.
#[tokio::test]
async fn a_merge_cant_discard_a_link_the_token_cant_touch() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let inflow = the_other_side(&world, 1_250).await;
    linked(&world, pending, inflow).await;
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("one side").unwrap(),
            Scope::Write,
            None,
            vec![TokenGrant {
                target: GrantTarget::Account(checking.id),
                level: AccountLevel::Write,
            }],
        )
        .await
        .unwrap();

    let (status, body) = send(
        &world,
        &token,
        &format!("/api/v1/transactions/{posted}/merge"),
        Some(json!({"duplicate": pending, "discard_duplicate_state": true})),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "merge_conflict");
    assert!(!body["detail"].as_str().unwrap().contains("link"));
    assert!(get(&world, pending).await.transfer_link.is_some());
}

/// An un-merge leaves the link on the transaction that holds it, and the new one starts bare.
#[tokio::test]
async fn an_unmerge_leaves_the_link_where_it_is() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let (pending, posted) = flagged_pair(&world, &link).await;
    let inflow = the_other_side(&world, 1_250).await;
    let made = linked(&world, posted, inflow).await;
    let (_, merged) = merge(&world, posted, json!({"duplicate": pending})).await;
    assert_eq!(merged["transfer_link"]["id"], made["id"]);
    let source = get(&world, posted)
        .await
        .sources
        .iter()
        .find(|source| source.fields.pending)
        .unwrap()
        .id;

    let (status, created) = unmerge(&world, posted, source, None).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["transfer_link"], Value::Null);
    let kept = get(&world, posted).await.transfer_link.unwrap();
    assert_eq!(kept.other, inflow);
}

/// Pending becoming posted is the same transaction: the link rides along.
#[tokio::test]
async fn a_leg_going_from_pending_to_posted_keeps_its_link() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", true)],
    )
    .await;
    let leg = holding(&world, &link, "pending-1").await.id;
    let inflow = the_other_side(&world, 1_250).await;
    linked(&world, leg, inflow).await;

    let mut posted = record("posted-1", 11, -1_250, "CORNER SHOP", false);
    posted.replaces_provider_id = Some("pending-1".into());
    apply(&world, &link, vec![posted]).await;

    let after = get(&world, leg).await;
    assert!(!after.pending);
    assert_eq!(after.transfer_link.unwrap().other, inflow);
}

/// Vanishing from the provider would delete a transaction holding nothing a person gave it. A
/// link is something a person gave it, so the leg stays, flagged, and its link with it: a
/// person decides, by merging its posted form into it or by removing the link.
#[tokio::test]
async fn a_leg_the_provider_removes_stays_with_its_link() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", true)],
    )
    .await;
    let leg = holding(&world, &link, "pending-1").await;
    let inflow = the_other_side(&world, 1_250).await;
    linked(&world, leg.id, inflow).await;

    vanish(&world, &link, "pending-1").await;

    let after = get(&world, leg.id).await;
    assert_eq!(after.flags, [TransactionFlag::SourceRemoved]);
    assert_eq!(after.transfer_link.unwrap().other, inflow);
    assert_eq!(after.sources, []);
}

/// The charge coming back under another record attaches to the leg, as to any transaction the
/// provider took back: the flag clears and the link was never in question.
#[tokio::test]
async fn a_removed_leg_coming_back_clears_its_flag() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let charge = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    apply(&world, &link, vec![charge]).await;
    let leg = holding(&world, &link, "pending-1").await.id;
    let inflow = the_other_side(&world, 1_250).await;
    linked(&world, leg, inflow).await;
    vanish(&world, &link, "pending-1").await;

    let posted = record("posted-1", 10, -1_250, "CORNER SHOP", false);
    apply(&world, &link, vec![posted]).await;

    let after = get(&world, leg).await;
    assert_eq!(holding(&world, &link, "posted-1").await.id, leg);
    assert_eq!(after.flags, []);
    assert_eq!(after.transfer_link.unwrap().other, inflow);
}

/// A posted form the sync can't place, in another amount here, is a person's to merge into the
/// leg, which then holds a record again, so isn't flagged as having none.
#[tokio::test]
async fn a_removed_leg_takes_a_posted_form_in_a_merge() {
    let world = world().await;
    let checking = account(&world, "Checking").await;
    let link = link(&world, &checking).await;
    let charge = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    apply(&world, &link, vec![charge]).await;
    let leg = holding(&world, &link, "pending-1").await.id;
    let inflow = the_other_side(&world, 1_250).await;
    linked(&world, leg, inflow).await;
    vanish(&world, &link, "pending-1").await;

    let posted = record("posted-1", 12, -1_300, "CORNER SHOP", false);
    apply(&world, &link, vec![posted]).await;
    let new = holding(&world, &link, "posted-1").await;
    assert_ne!(new.id, leg, "another amount is another transaction");

    let (status, body) = merge(&world, leg, json!({"duplicate": new.id})).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["transfer_link"]["other_transaction"], json!(inflow));
    assert_eq!(body["flags"], json!([]));
}

/// A pass reporting `provider_id` as removed, with nothing else.
async fn vanish(world: &World, link: &Link, provider_id: &str) {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    apply_account_batch(
        &mut tx,
        now,
        link.connection,
        AccountBatch {
            provider_account: link.provider_account.clone(),
            account: Some(link.account.clone()),
            transactions: Vec::new(),
            removed: vec![provider_id.to_owned()],
            coverage: Coverage::Delta,
            unusable: Vec::new(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
