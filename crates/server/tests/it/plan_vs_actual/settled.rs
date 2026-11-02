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

/// 150.00 spent in Food, with a 100.00 bill and a 300.00 budget. The bill takes its 100.00
/// first, so 50.00 went on the budget. Settled, the spending wasn't for it: all 150.00 did,
/// as the forecast has it, and the row still plans both.
#[tokio::test]
async fn a_settled_bill_takes_no_share_of_the_rollover() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let kit = world
        .item(&monthly("Kit", "outflow", 10_000, food, "2026-01-01"))
        .await;
    world.item(&budget("Groceries", 30_000, food)).await;
    world.posted(checking, "2026-11-05", -15_000, food).await;
    let before = world.report("from=2026-11&months=1").await;
    assert_eq!(before["rollovers"][0]["spent"], usd(5_000));

    world.settle(&kit, "2026-11", "paid").await;

    let report = world.report("from=2026-11&months=1").await;
    let rollover = &report["rollovers"][0];
    assert_eq!(rollover["budgeted"], usd(30_000));
    assert_eq!(rollover["spent"], usd(15_000));
    assert_eq!(rollover["rollover"], usd(15_000));
    let row = row(&report, "2026-11", food);
    assert_eq!(row["planned"], usd(40_000));
    assert_eq!(row["settled"], usd(10_000));
}
