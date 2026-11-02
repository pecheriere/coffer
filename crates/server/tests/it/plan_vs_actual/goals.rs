//! A split linked to a goal counts against the goal, not as an actual of its category
//! (spec §6.9), so planned big-ticket spending never shows up as category overspend.

use axum::http::Method;
use serde_json::{Value, json};

use super::{DUPLICATE, REMOVED, REVISED, World, goal, is_empty, row, usd, world};

impl World {
    /// The one line of `report`'s goals.
    fn goal_line(report: &Value) -> &Value {
        let lines = report["goals"].as_array().unwrap();
        let [only] = lines.as_slice() else {
            panic!("expected exactly one goal line");
        };
        only
    }
}

/// Of the 365 days to 2027, 273 have passed on October 1st and 304 on November 1st:
/// round(1,000,000 × 304 / 365) − round(1,000,000 × 273 / 365) = 832,877 − 747,945.
#[tokio::test]
async fn goal_split_counts_against_the_goal_not_the_category() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000_000, home)).await;
    world
        .posted_linked(checking, "2026-10-06", -300_000, home, &roof)
        .await;
    world.posted(checking, "2026-10-07", -50, home).await;

    let report = world.october().await;

    assert_eq!(row(&report, "2026-10", home)["actual"], usd(50));
    assert_eq!(
        report["goals"],
        json!([{
            "month": "2026-10",
            "goal": roof,
            "name": "Roof",
            "currency": "USD",
            "accrued": usd(84_932),
            "spent_against": usd(300_000),
            "left_out": usd(0),
        }])
    );
}

#[tokio::test]
async fn goal_spending_stays_in_its_own_month() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000, home)).await;
    world
        .posted_linked(checking, "2026-09-30", -1, home, &roof)
        .await;
    world
        .posted_linked(checking, "2026-10-01", -10, home, &roof)
        .await;
    world
        .posted_linked(checking, "2026-11-01", -100, home, &roof)
        .await;

    let report = world.october().await;

    assert_eq!(World::goal_line(&report)["spent_against"], usd(10));
}

/// A refund on a goal nets, and flagged it keeps counting, as in the forecast.
#[tokio::test]
async fn a_refund_on_a_goal_nets_even_when_flagged() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000, home)).await;
    world
        .posted_linked(checking, "2026-10-06", -500, home, &roof)
        .await;
    let refund = world
        .posted_linked(checking, "2026-10-07", 30, home, &roof)
        .await;
    world.plant(REMOVED, &refund).await;

    let report = world.october().await;

    let line = World::goal_line(&report);
    assert_eq!(line["spent_against"], usd(470));
    assert_eq!(line["left_out"], usd(0));
}

/// R10a: removed, revised or possibly another charge again, a charge doesn't count
/// against a goal, and the line says how much didn't.
#[tokio::test]
async fn a_flagged_goal_charge_is_left_out_and_reported() {
    for flag in [REMOVED, REVISED, DUPLICATE] {
        let world = world().await;
        let checking = world.checking().await;
        let home = world.expense("Home").await;
        let roof = world.item(&goal("Roof", 1_000, home)).await;
        world
            .posted_linked(checking, "2026-10-06", -30, home, &roof)
            .await;
        let flagged = world
            .posted_linked(checking, "2026-10-07", -500, home, &roof)
            .await;
        world.plant(flag, &flagged).await;

        let report = world.october().await;

        let line = World::goal_line(&report);
        assert_eq!(line["spent_against"], usd(30), "the flag leaves it out");
        assert_eq!(line["left_out"], usd(500), "and says how much");
    }
}

/// Moving money to savings for a goal isn't spending it. A split can't be linked in a
/// `transfer` category, so this one was linked in an `expense` one that then changed.
#[tokio::test]
async fn a_goal_linked_transfer_is_not_goal_spending() {
    let world = world().await;
    let checking = world.checking().await;
    let moved = world.expense("Moved").await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000, home)).await;
    world
        .posted_linked(checking, "2026-10-06", -500, moved, &roof)
        .await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/categories/{moved}"),
            &json!({"kind": "transfer"}),
        )
        .await;

    let report = world.october().await;

    assert_eq!(World::goal_line(&report)["spent_against"], usd(0));
    assert!(is_empty(&report, "rows"), "nor is it a category's");
}

/// Its split counts nowhere else, so the line stays though the goal plans nothing more.
#[tokio::test]
async fn an_inactive_goal_keeps_a_line_for_its_spending() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000_000, home)).await;
    world
        .posted_linked(checking, "2026-10-06", -500, home, &roof)
        .await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/plan-items/{}", roof.as_str().unwrap()),
            &json!({"active": false}),
        )
        .await;

    let report = world.october().await;

    let line = World::goal_line(&report);
    assert_eq!(line["spent_against"], usd(500));
    assert_eq!(line["accrued"], usd(0), "and accrues nothing");
}

#[tokio::test]
async fn an_inactive_goal_without_spending_has_no_line() {
    let world = world().await;
    let home = world.expense("Home").await;
    let mut roof = goal("Roof", 1_000_000, home);
    roof["active"] = json!(false);
    world.item(&roof).await;

    assert!(is_empty(&world.october().await, "goals"), "nothing to say");
}

/// Goal money outside the report's months doesn't keep an inactive goal's line.
#[tokio::test]
async fn an_inactive_goal_with_spending_elsewhere_has_no_line() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000_000, home)).await;
    world
        .posted_linked(checking, "2026-08-06", -500, home, &roof)
        .await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/plan-items/{}", roof.as_str().unwrap()),
            &json!({"active": false}),
        )
        .await;

    assert!(
        is_empty(&world.october().await, "goals"),
        "nothing in October"
    );
}
