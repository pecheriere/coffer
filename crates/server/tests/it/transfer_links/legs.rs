//! What a leg may hold (spec §8.12 T4): one category at most, and no goal while the money
//! stays in counted cash.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::CategoryId;
use serde_json::{Value, json};

use super::{LINKS, TRANSACTIONS, World, linking, world};

/// A goal of 1,000.00 in `category`.
fn a_goal(category: CategoryId) -> Value {
    json!({
        "name": "Roof",
        "category": category,
        "amount": {"minor_units": 100_000, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "goal", "start_on": "2026-01-01", "due_on": "2027-01-01"},
    })
}

impl World {
    async fn patched_splits(&self, transaction: &str, splits: &Value) -> StatusCode {
        self.send(
            Method::PATCH,
            &format!("{TRANSACTIONS}/{transaction}"),
            &self.admin,
            Some(&json!({"splits": splits})),
        )
        .await
        .0
    }

    async fn goal_in(&self, category: CategoryId) -> Value {
        self.admin_send(Method::POST, "/api/v1/plan-items", &a_goal(category))
            .await["id"]
            .clone()
    }
}

/// Part of a split transaction may be a real purchase, and a report can't tell which, so a
/// transaction of two splits can't be a leg, and a leg can't become one.
#[tokio::test]
async fn a_split_transaction_cant_be_a_leg() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let food = world.category("Food", "expense").await;
    let two = json!([
        {"category": food, "minor_units": -20_000},
        {"category": food, "minor_units": -30_000},
    ]);
    world.splits(&outflow, &two).await;

    let (status, _) = world
        .send(
            Method::POST,
            LINKS,
            &world.admin,
            Some(&linking(&outflow, &inflow)),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    world
        .splits(
            &outflow,
            &json!([{"category": food, "minor_units": -50_000}]),
        )
        .await;
    world.link(&outflow, &inflow).await;

    assert_eq!(
        world.patched_splits(&outflow, &two).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        world
            .patched_splits(
                &outflow,
                &json!([{"category": food, "minor_units": -50_000}])
            )
            .await,
        StatusCode::OK,
        "one category is still the member's to change"
    );
    assert_eq!(
        world.patched_splits(&outflow, &json!([])).await,
        StatusCode::OK
    );
}

/// Money moved to savings for a goal is still cash (R8), linked or not.
#[tokio::test]
async fn a_goal_stays_off_a_transfer() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let home = world.category("Home", "expense").await;
    let moved = world.category("Moved", "transfer").await;
    let goal = world.goal_in(home).await;
    let on_the_goal = json!([{"category": home, "minor_units": -50_000, "goal": goal}]);

    world.splits(&outflow, &on_the_goal).await;
    let (status, _) = world
        .send(
            Method::POST,
            LINKS,
            &world.admin,
            Some(&linking(&outflow, &inflow)),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "linked to a goal");

    world
        .splits(
            &outflow,
            &json!([{"category": home, "minor_units": -50_000}]),
        )
        .await;
    world.link(&outflow, &inflow).await;

    assert_eq!(
        world.patched_splits(&outflow, &on_the_goal).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a goal after the link"
    );
    assert_eq!(
        world
            .patched_splits(
                &outflow,
                &json!([{"category": moved, "minor_units": -50_000, "goal": goal}])
            )
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the refusal of a goal on a transfer category stands"
    );
}

/// Money leaving counted cash is spending, and a contribution to a 529 or a brokerage may count
/// towards its goal (B10), so such a link refuses no goal, either way round.
#[tokio::test]
async fn a_goal_stays_allowed_on_money_leaving_counted_cash() {
    let world = world().await;
    let fund = world.account("Brokerage", "brokerage", "USD").await;
    let tuition = world.category("Tuition", "expense").await;
    let goal = world.goal_in(tuition).await;
    let on_the_goal = json!([{"category": tuition, "minor_units": -50_000, "goal": goal}]);

    let first = world.manual(world.checking, "2026-10-05", -50_000).await;
    world.splits(&first, &on_the_goal).await;
    let inflow = world.manual(fund, "2026-10-05", 50_000).await;
    world.link(&first, &inflow).await;

    let second = world.manual(world.checking, "2026-10-06", -50_000).await;
    let inflow = world.manual(fund, "2026-10-06", 50_000).await;
    world.link(&second, &inflow).await;
    assert_eq!(
        world.patched_splits(&second, &on_the_goal).await,
        StatusCode::OK
    );
}
