//! A settled occurrence in plan versus actual, through the router (spec §8.9 R9a): it
//! stays in `planned`, since it is what was planned, and the report says it is settled.

use axum::http::Method;
use serde_json::{Value, json};

use super::{ITEMS, World, budget, monthly, row, usd, world};

impl World {
    async fn settle(&self, item: &Value, month: &str, kind: &str) {
        let uri = format!("{ITEMS}/{}/settlements/{month}", item.as_str().unwrap());
        self.admin_send(Method::PUT, &uri, &json!({"kind": kind}))
            .await;
    }
}

/// Internet, 80.00 on the 10th, paid from an account coffer doesn't track: nothing to
/// see in `actual`, 80.00 of `difference`, and the report says why that is no surprise.
#[tokio::test]
async fn a_settled_occurrence_stays_planned_and_shows_as_settled() {
    let world = world().await;
    let utilities = world.expense("Utilities").await;
    let internet = world
        .item(&monthly(
            "Internet",
            "outflow",
            8_000,
            utilities,
            "2026-01-10",
        ))
        .await;

    world.settle(&internet, "2026-11", "paid").await;

    let report = world.report("from=2026-10&months=2").await;
    let settled_month = row(&report, "2026-11", utilities);
    assert_eq!(settled_month["planned"], usd(8_000));
    assert_eq!(settled_month["difference"], usd(8_000));
    assert_eq!(settled_month["settled"], usd(8_000));
    assert_eq!(row(&report, "2026-10", utilities)["settled"], usd(0));
    assert_eq!(
        report["settled"],
        json!([{
            "month": "2026-11",
            "item": internet,
            "name": "Internet",
            "category": utilities,
            "kind": "paid",
            "amount": usd(8_000),
        }])
    );
}

/// An inactive item plans nothing, so it has nothing settled to show.
#[tokio::test]
async fn an_inactive_item_shows_nothing_settled() {
    let world = world().await;
    let utilities = world.expense("Utilities").await;
    let internet = world
        .item(&monthly(
            "Internet",
            "outflow",
            8_000,
            utilities,
            "2026-01-10",
        ))
        .await;
    world.settle(&internet, "2026-11", "paid").await;
    let uri = format!("{ITEMS}/{}", internet.as_str().unwrap());
    world
        .admin_send(Method::PATCH, &uri, &json!({"active": false}))
        .await;

    let report = world.report("from=2026-11&months=1").await;

    assert_eq!(report["settled"], json!([]));
}

/// A rollover is for budgets, and settling is for bills, so the settlement changes it not
/// at all: 300.00 budgeted, 150.00 spent less today's 100.00 bill, either way.
#[tokio::test]
async fn settling_a_bill_leaves_the_rollover_alone() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let kit = world
        .item(&monthly("Kit", "outflow", 10_000, food, "2026-01-01"))
        .await;
    world.item(&budget("Groceries", 30_000, food)).await;
    world.posted(checking, "2026-11-05", -15_000, food).await;
    let before = world.report("from=2026-11&months=1").await["rollovers"].clone();
    assert_eq!(before[0]["spent"], usd(5_000));

    world.settle(&kit, "2026-11", "paid").await;

    let report = world.report("from=2026-11&months=1").await;
    assert_eq!(report["rollovers"], before);
}
