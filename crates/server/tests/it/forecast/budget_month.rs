//! R9 and R19 through the real router: what the store counts through `as_of` reaches the
//! calculation, which holds a budget's whole month and leaves a paid bill's spending to it.

use serde_json::json;

use super::{AS_OF, World, kinds, line, monthly, outflow, usd, with, world};

async fn budget(
    world: &World,
    name: &str,
    minor_units: i64,
    anchor_on: &str,
) -> coffer_domain::ids::CategoryId {
    let category = world.expense("Home").await;
    world
        .item(&with(
            outflow(name, minor_units, category, &monthly(anchor_on)),
            "spending",
            json!("budget"),
        ))
        .await;
    category
}

/// Groceries of 80,000 on the 1st with 50,000 spent: 30,000 of November, then December,
/// January and February. 2,000,000 - 500,000 - 270,000.
#[tokio::test]
async fn a_budget_dated_earlier_this_month_holds_back_what_is_unspent() {
    let world = world().await;
    let checking = world.funded("Checking", 2_000_000).await;
    world.floor(500_000).await;
    let food = budget(&world, "Groceries", 80_000, "2026-01-01").await;
    world.spend(checking, "2026-11-02", 50_000, food).await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Groceries")["counted"], usd(270_000));
    assert_eq!(forecast["surplus"], usd(1_230_000));
    assert_eq!(forecast["status"], "firm");
}

/// A budget is a month's envelope for its category: spending before its first occurrence,
/// here the month it starts, still reduces it.
#[tokio::test]
async fn spending_before_a_budgets_first_occurrence_this_month_reduces_it() {
    let world = world().await;
    let checking = world.funded("Checking", 2_000_000).await;
    world.floor(0).await;
    let food = budget(&world, "Groceries", 80_000, "2026-11-20").await;
    world.spend(checking, "2026-11-02", 50_000, food).await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Groceries")["reduced_by"], usd(50_000));
}

/// Rent of 200,000 paid today with a balance dated today, and 5,000 more spent: what is
/// spent on `as_of` reaches the pool, and the payment of the rent leaves it. §10.8: the balance no
/// longer holds the rent, so November's needs nothing, and only December's and January's count.
#[tokio::test]
async fn a_bill_paid_on_as_of_leaves_only_the_rest_to_a_budget() {
    let world = world().await;
    let checking = world.funded("Checking", 2_000_000).await;
    world.balance(checking, AS_OF, 1_795_000).await;
    world.floor(0).await;
    let home = budget(&world, "Upkeep", 20_000, "2026-01-20").await;
    world
        .item(&outflow("Rent", 200_000, home, &monthly("2026-11-15")))
        .await;
    world.spend(checking, AS_OF, 200_000, home).await;
    world.spend(checking, AS_OF, 5_000, home).await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Upkeep")["reduced_by"], usd(5_000));
    assert_eq!(line(&forecast, "Rent")["counted"], usd(400_000));
}

#[tokio::test]
async fn a_future_as_of_does_not_age_a_balance_dated_today() {
    let world = world().await;
    let checking = world.funded("Checking", 1_000).await;
    world.balance(checking, AS_OF, 1_000).await;

    let forecast = world.forecast_at("?as_of=2026-12-15").await;

    assert!(!kinds(&forecast, "cautions").contains(&"balance_stale".to_owned()));
}
