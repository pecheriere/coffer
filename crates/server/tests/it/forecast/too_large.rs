//! A sum past `i64` in what the forecast reads refuses the answer (spec §8.9 R20), never
//! a 500, whichever of its queries meets it. The account has a balance, so each query
//! has something to read.

use axum::http::{Method, StatusCode};
use serde_json::Value;

use super::{BALANCE_ON, FORECAST, World, goal_schedule, outflow, world};

const TOO_LARGE: &str = "the forecast's totals are too large to represent";

/// Two amounts whose sum overflows, and two that sum to exactly `i64::MIN`, the one sum
/// that fits until it's negated. Positive for money out.
const PAST_THE_RANGE: [[i64; 2]; 2] = [
    [9_000_000_000_000_000_000, 9_000_000_000_000_000_000],
    [9_000_000_000_000_000_000, 223_372_036_854_775_808],
];

impl World {
    async fn assert_too_large(&self) {
        let (status, text) = self.send(Method::GET, FORECAST, &self.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["type"], "urn:coffer:problem:invalid");
        assert_eq!(body["detail"], TOO_LARGE);
    }
}

/// `CATEGORY_SPENT`: categorized, and on or before the balance's date.
#[tokio::test]
async fn category_spending_too_large_is_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let checking = world.funded("Checking", 1_000).await;
        let food = world.expense("Food").await;
        for amount in amounts {
            world.spend(checking, BALANCE_ON, amount, food).await;
        }
        world.assert_too_large().await;
    }
}

/// `GOAL_SPENT`: linked to a goal, and on or before the balance's date.
#[tokio::test]
async fn goal_spending_too_large_is_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let checking = world.funded("Checking", 1_000).await;
        let home = world.expense("Home").await;
        let roof = world
            .item(&outflow(
                "Roof",
                1_000,
                home,
                &goal_schedule("2026-01-01", "2027-01-01"),
            ))
            .await;
        for amount in amounts {
            world
                .spend_linked(checking, BALANCE_ON, amount, home, &roof)
                .await;
        }
        world.assert_too_large().await;
    }
}

/// `LOOSE_ENDS`: uncategorized, and on or before the balance's date.
#[tokio::test]
async fn uncategorized_spending_too_large_is_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let checking = world.funded("Checking", 1_000).await;
        for amount in amounts {
            world.manual(checking, BALANCE_ON, -amount).await;
        }
        world.assert_too_large().await;
    }
}

/// `NEWER_SPENDING`: after the balance's date.
#[tokio::test]
async fn newer_spending_too_large_is_422() {
    for amounts in PAST_THE_RANGE {
        let world = world().await;
        let checking = world.funded("Checking", 1_000).await;
        let food = world.expense("Food").await;
        for amount in amounts {
            world.spend(checking, super::AS_OF, amount, food).await;
        }
        world.assert_too_large().await;
    }
}
