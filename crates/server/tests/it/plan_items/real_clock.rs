//! On the real clock, which keeps finer than the store does, a save answers with the time a
//! later read shows: nobody comparing the two sees a change that never happened.

use axum::http::{Method, StatusCode};
use coffer_domain::time::{Clock as _, SystemClock};
use coffer_server::testkit::TestServer;
use serde_json::{Value, json};

use super::{World, item_uri, recurring, world_on};

async fn real_world() -> World {
    world_on(TestServer::with_system_clock().await).await
}

async fn read(world: &World, id: &str) -> Value {
    let (status, item) = world
        .call(Method::GET, &item_uri(id), &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    item
}

#[tokio::test]
async fn a_new_plan_item_answers_with_the_times_a_later_read_shows() {
    let world = real_world().await;

    let (status, created) = world
        .call(
            Method::POST,
            super::ITEMS,
            &world.admin,
            Some(&recurring(world.expense)),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    let read_back = read(&world, created["id"].as_str().unwrap()).await;
    assert_eq!(read_back["created_at"], created["created_at"]);
    assert_eq!(read_back["updated_at"], created["updated_at"]);
}

#[tokio::test]
async fn a_settlement_answers_with_the_time_a_later_read_shows() {
    let world = real_world().await;
    let body = json!({
        "name": "Rent",
        "category": world.expense,
        "amount": {"minor_units": 4200, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "recurring", "every": 1, "unit": "months",
                     "anchor_on": "2000-01-01"},
    });
    let (_, item) = world
        .call(Method::POST, super::ITEMS, &world.admin, Some(&body))
        .await;
    let id = item["id"].as_str().unwrap();

    let this_month = SystemClock.now().strftime("%Y-%m").to_string();

    let (status, settled) = world
        .call(
            Method::PUT,
            &format!("{}/settlements/{this_month}", item_uri(id)),
            &world.admin,
            Some(&json!({"kind": "paid"})),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let read_back = read(&world, id).await;
    assert_eq!(
        read_back["settlements"][0]["settled_at"],
        settled["settled_at"]
    );
}
