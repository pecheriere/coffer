//! What a change reaches, on the same server with no restart: each answer that asks what
//! today or this month is.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{AUCKLAND, AUCKLAND_DAY, LOS_ANGELES, LOS_ANGELES_DAY, set_zone, world_at};
use crate::net_worth::World;

async fn get(world: &World, uri: &str) -> Value {
    let (status, text) = world.send(Method::GET, uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_str(&text).unwrap()
}

/// Applies each choice in turn and checks `read` follows it, the last being a return to
/// the server's default.
async fn follows(uri: &str, read: impl Fn(&Value) -> Value, auckland: Value, los_angeles: Value) {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    assert_eq!(read(&get(&world, uri).await), los_angeles, "the default");

    set_zone(&world, json!(AUCKLAND)).await;
    assert_eq!(read(&get(&world, uri).await), auckland, "chosen");

    set_zone(&world, json!(LOS_ANGELES)).await;
    assert_eq!(read(&get(&world, uri).await), los_angeles, "chosen again");

    set_zone(&world, json!(AUCKLAND)).await;
    set_zone(&world, Value::Null).await;
    assert_eq!(read(&get(&world, uri).await), los_angeles, "cleared");
}

#[tokio::test]
async fn the_forecast_s_day_follows_it() {
    follows(
        "/api/v1/forecast",
        |forecast| forecast["as_of"].clone(),
        json!(AUCKLAND_DAY),
        json!(LOS_ANGELES_DAY),
    )
    .await;
}

#[tokio::test]
async fn the_plan_report_s_current_month_follows_it() {
    follows(
        "/api/v1/reports/plan-vs-actual?from=2026-10&months=2",
        |report| report["partial_months"].clone(),
        json!(["2026-11"]),
        json!(["2026-10"]),
    )
    .await;
}

#[tokio::test]
async fn net_worth_s_today_follows_it() {
    follows(
        "/api/v1/reports/net-worth",
        |report| report["to"].clone(),
        json!(AUCKLAND_DAY),
        json!(LOS_ANGELES_DAY),
    )
    .await;
}
