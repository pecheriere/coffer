//! A link survives whatever rewrites its split: a sync, a merge, an unmerge.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::super::{World, goal, world};
use super::{TRANSACTIONS, provider_record};

/// A transaction joined from two records and linked to a goal, with its posted
/// record unmerged. Returns its id, the goal's, and the transaction created.
async fn unmerged_with_a_link(world: &World) -> (String, String, Value) {
    let feed = world.feed().await;
    world
        .apply(&feed, vec![provider_record("AUTHORIZED", -4_000, true)])
        .await;
    let mut settled = provider_record("SETTLED", -4_600, false);
    settled.replaces_provider_id = Some("AUTHORIZED".into());
    world.apply(&feed, vec![settled]).await;
    let joined = world.imported(&feed, "SETTLED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&joined, -4_600, &goal_id).await;
    let read = world.read_transaction(&joined).await;
    let source = read["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["pending"] == false)
        .unwrap()["id"]
        .clone();

    let uri = format!("{TRANSACTIONS}/{joined}/unmerge");
    let body = json!({"source": source});
    let (status, created) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    (joined, goal_id, created)
}

#[tokio::test]
async fn imported_amount_change_keeps_goal_link() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(&feed, vec![provider_record("BOOKING", -1_000, false)])
        .await;
    let transaction = world.imported(&feed, "BOOKING").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -1_000, &goal_id).await;

    world
        .apply(&feed, vec![provider_record("BOOKING", -1_100, false)])
        .await;

    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["amount"]["minor_units"], -1_100);
    assert_eq!(read["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn merge_keeps_goal_links() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(
            &feed,
            vec![
                provider_record("AUTHORIZED", -1_000, true),
                provider_record("SETTLED", -1_100, false),
            ],
        )
        .await;
    let target = world.imported(&feed, "AUTHORIZED").await;
    let duplicate = world.imported(&feed, "SETTLED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&target, -1_000, &goal_id).await;

    let uri = format!("{TRANSACTIONS}/{target}/merge");
    let body = json!({"duplicate": duplicate});
    let (status, _) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&target).await;
    assert_eq!(read["splits"][0]["amount"]["minor_units"], -1_100);
    assert_eq!(read["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn merging_away_a_linked_duplicate_frees_the_goal() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(
            &feed,
            vec![
                provider_record("AUTHORIZED", -1_000, true),
                provider_record("SETTLED", -1_000, false),
            ],
        )
        .await;
    let target = world.imported(&feed, "SETTLED").await;
    let duplicate = world.imported(&feed, "AUTHORIZED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&duplicate, -1_000, &goal_id).await;

    let uri = format!("{TRANSACTIONS}/{target}/merge");
    let body = json!({"duplicate": duplicate, "discard_duplicate_state": true});
    let (status, _) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(world.delete_item(&goal_id).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unmerge_keeps_goal_links() {
    let world = world().await;
    let (joined, goal_id, _) = unmerged_with_a_link(&world).await;

    let kept = world.read_transaction(&joined).await;
    assert_eq!(kept["splits"][0]["amount"]["minor_units"], -4_000);
    assert_eq!(kept["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn unmerge_leaves_the_new_transaction_without_splits() {
    let world = world().await;
    let (_, _, created) = unmerged_with_a_link(&world).await;

    assert_eq!(created["splits"], json!([]));
    let id = created["id"].as_str().unwrap();
    let read = world.read_transaction(id).await;
    assert_eq!(read["splits"], json!([]));
}
