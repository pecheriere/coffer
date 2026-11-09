//! Plan history through the API (money redesign §10.3): a change applies from this month or
//! next, never to a past month, and the item answers its versions.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{goal, one_off, recurring, with, world};

const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;
const BAD_REQUEST: StatusCode = StatusCode::BAD_REQUEST;

fn euros(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "USD"})
}

async fn world_on_november_10() -> super::World {
    let world = world().await;
    world
        .server
        .clock
        .set("2026-11-10T12:00:00Z".parse().unwrap());
    world
}

#[tokio::test]
async fn a_change_applies_from_this_month_by_default() {
    let world = world_on_november_10().await;
    let id = world.create(&recurring(world.expense)).await;

    let after = world.patch(&id, &json!({"amount": euros(7_700)})).await;

    assert_eq!(after["amount"]["minor_units"], json!(7_700));
    let versions = after["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["until"], json!("2026-11"));
    assert_eq!(versions[1]["from"], json!("2026-11"));
    assert_eq!(versions[1]["amount"]["minor_units"], json!(7_700));
}

#[tokio::test]
async fn next_month_leaves_this_month_alone() {
    let world = world_on_november_10().await;
    let id = world.create(&recurring(world.expense)).await;
    let before = world.patch(&id, &json!({})).await;

    let after = world
        .patch(
            &id,
            &json!({"amount": euros(7_700), "effective_from": "next_month"}),
        )
        .await;

    assert_eq!(
        after["amount"], before["amount"],
        "this month still reads the old amount"
    );
    let versions = after["versions"].as_array().unwrap();
    assert_eq!(versions[1]["from"], json!("2026-12"));
    assert_eq!(versions[1]["amount"]["minor_units"], json!(7_700));
}

#[tokio::test]
async fn a_past_month_is_422() {
    let world = world_on_november_10().await;
    let id = world.create(&recurring(world.expense)).await;

    let message = world
        .patch_refused(
            &id,
            &json!({"amount": euros(1), "effective_from": "2026-10"}),
            UNPROCESSABLE,
        )
        .await;
    assert!(
        message.contains("only this month's or next month's plan can change"),
        "{message}"
    );
    world
        .patch_refused(
            &id,
            &json!({"amount": euros(1), "effective_from": "later"}),
            BAD_REQUEST,
        )
        .await;
    let after = world
        .patch(
            &id,
            &json!({"amount": euros(1), "effective_from": "2026-12"}),
        )
        .await;
    assert_eq!(after["versions"][1]["from"], json!("2026-12"));
}

#[tokio::test]
async fn next_month_on_a_goal_or_one_off_is_422() {
    let world = world_on_november_10().await;
    for body in [goal(world.expense), one_off(world.expense)] {
        let id = world.create(&body).await;
        let message = world
            .patch_refused(
                &id,
                &json!({"amount": euros(1), "effective_from": "next_month"}),
                UNPROCESSABLE,
            )
            .await;
        assert!(
            message.contains("only a recurring item can change from next month"),
            "{message}"
        );
    }
}

#[tokio::test]
async fn a_past_one_off_keeps_its_amount() {
    let world = world_on_november_10().await;
    let past = with(
        one_off(world.expense),
        "schedule",
        json!({"type": "one_off", "on": "2026-09-01"}),
    );
    let id = world.create(&past).await;

    let message = world
        .patch_refused(&id, &json!({"amount": euros(1)}), UNPROCESSABLE)
        .await;

    assert!(
        message.contains("a one-off in a past month keeps its amount"),
        "{message}"
    );
}

#[tokio::test]
async fn effective_from_alone_changes_nothing() {
    let world = world_on_november_10().await;
    let id = world.create(&recurring(world.expense)).await;
    let audited = world.audit().await.len();

    let after = world
        .patch(&id, &json!({"effective_from": "next_month"}))
        .await;

    assert_eq!(after["versions"].as_array().unwrap().len(), 1);
    assert_eq!(world.audit().await.len(), audited, "nothing is written");
}
