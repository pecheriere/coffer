//! A budget's rollover covers only its life, through the router (spec §8.9 R6).

use serde_json::{Value, json};

use super::{budget, usd, world};

/// The one rollover of `report`.
fn only_rollover(report: &Value) -> &Value {
    let lines = report["rollovers"].as_array().unwrap();
    let [only] = lines.as_slice() else {
        panic!("expected exactly one rollover");
    };
    only
}

/// August and September's 500s were before there was a budget to spend.
#[tokio::test]
async fn spending_before_a_budget_starts_is_not_charged_to_it() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let mut groceries = budget("Groceries", 100, food);
    groceries["schedule"]["anchor_on"] = json!("2026-10-10");
    world.item(&groceries).await;
    for (on, minor_units) in [
        ("2026-08-12", -500),
        ("2026-09-12", -500),
        ("2026-10-12", -50),
        ("2026-11-12", -30),
    ] {
        world.posted(checking, on, minor_units, food).await;
    }

    let report = world.report("from=2026-08&months=4").await;

    let line = only_rollover(&report);
    assert_eq!(line["budgeted"], usd(200));
    assert_eq!(line["spent"], usd(80));
    assert_eq!(line["rollover"], usd(120));
}

/// October and November's 500s came after the budget ended.
#[tokio::test]
async fn spending_after_a_budget_ends_is_not_charged_to_it() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let mut groceries = budget("Groceries", 100, food);
    groceries["schedule"]["anchor_on"] = json!("2026-08-10");
    groceries["schedule"]["end_on"] = json!("2026-09-30");
    world.item(&groceries).await;
    for (on, minor_units) in [
        ("2026-08-12", -50),
        ("2026-09-12", -60),
        ("2026-10-12", -500),
        ("2026-11-12", -500),
    ] {
        world.posted(checking, on, minor_units, food).await;
    }

    let report = world.report("from=2026-08&months=4").await;

    let line = only_rollover(&report);
    assert_eq!(line["budgeted"], usd(200));
    assert_eq!(line["spent"], usd(110));
    assert_eq!(line["rollover"], usd(90));
}
