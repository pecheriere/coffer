//! Totals past `i64` refuse the report (spec §8.9 R20), never a 500 or a wrapped number:
//! in the database's sums, and in the arithmetic after them.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::{ITEMS, World, goal, usd, world};

const TOO_LARGE: &str = "the report's totals are too large to represent";

/// Two amounts whose sum overflows, and two that sum to exactly `i64::MIN`, the one sum
/// that fits until it's negated.
const PAST_THE_RANGE: [[i64; 2]; 2] = [
    [-9_000_000_000_000_000_000, -9_000_000_000_000_000_000],
    [-9_000_000_000_000_000_000, -223_372_036_854_775_808],
];

impl World {
    async fn assert_too_large(&self) {
        let uri = "/api/v1/reports/plan-vs-actual?from=2026-10&months=1&average_months=1";
        let (status, text) = self.send(Method::GET, uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["type"], "urn:coffer:problem:invalid");
        assert_eq!(body["detail"], TOO_LARGE);
    }
}

#[tokio::test]
async fn category_splits_too_large_are_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let (checking, food) = world.food_on_checking().await;
        for amount in amounts {
            world.posted(checking, "2026-10-06", amount, food).await;
        }
        world.assert_too_large().await;
    }
}

#[tokio::test]
async fn left_out_splits_too_large_are_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let (checking, food) = world.food_on_checking().await;
        for amount in amounts {
            let id = world.posted(checking, "2026-10-06", amount, food).await;
            world.plant(super::REMOVED, &id).await;
        }
        world.assert_too_large().await;
    }
}

#[tokio::test]
async fn goal_splits_too_large_are_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let (checking, home) = world.food_on_checking().await;
        let roof = world.item(&goal("Roof", 1_000, home)).await;
        for amount in amounts {
            world
                .posted_linked(checking, "2026-10-06", amount, home, &roof)
                .await;
        }
        world.assert_too_large().await;
    }
}

#[tokio::test]
async fn uncategorized_outflows_too_large_are_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let checking = world.checking().await;
        for amount in amounts {
            world.manual(checking, "2026-10-06", amount).await;
        }
        world.assert_too_large().await;
    }
}

/// 298 items of 10^15 a day, in a 31-day month, plan 9.238 × 10^18: past `i64`.
#[tokio::test]
async fn planned_too_large_is_422() {
    let world = world().await;
    let food = world.expense("Food").await;
    let daily = json!({
        "name": "Daily",
        "category": food,
        "amount": usd(1_000_000_000_000_000),
        "direction": "outflow",
        "schedule": {"type": "recurring", "every": 1, "unit": "days", "anchor_on": "2026-01-01"},
    });
    for _ in 0..298 {
        world.admin_send(Method::POST, ITEMS, &daily).await;
    }
    world.assert_too_large().await;
}

/// Planned and actual each fit, but not the difference: a refund of 9.2 × 10^18 under a
/// plan of 3.1 × 10^16.
#[tokio::test]
async fn difference_too_large_is_422() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world
        .admin_send(
            Method::POST,
            ITEMS,
            &json!({
                "name": "Daily",
                "category": food,
                "amount": usd(1_000_000_000_000_000),
                "direction": "outflow",
                "schedule": {
                    "type": "recurring", "every": 1, "unit": "days", "anchor_on": "2026-01-01",
                },
            }),
        )
        .await;
    world
        .posted(checking, "2026-10-06", 9_200_000_000_000_000_000, food)
        .await;
    world.assert_too_large().await;
}
