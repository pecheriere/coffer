//! A flagged transaction stays visible (spec §8.9 R6, R10a): what stays out of `actual`
//! is reported beside it, and what still counts says so where it might mislead.

use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::json;

use super::{DUPLICATE, REMOVED, REVISED, World, budget, is_empty, row, usd, world};

impl World {
    pub(super) async fn food_on_checking(&self) -> (AccountId, CategoryId) {
        (self.checking().await, self.expense("Food").await)
    }
}

#[tokio::test]
async fn a_removed_charge_is_left_out_and_reported() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world.posted(checking, "2026-10-06", -30, food).await;
    let removed = world.posted(checking, "2026-10-07", -500, food).await;
    world.plant(REMOVED, &removed).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(30));
    assert_eq!(row["left_out"], usd(500));
}

#[tokio::test]
async fn a_revised_charge_is_left_out_and_reported() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world.posted(checking, "2026-10-06", -30, food).await;
    let revised = world.posted(checking, "2026-10-07", -500, food).await;
    world.plant(REVISED, &revised).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(30));
    assert_eq!(row["left_out"], usd(500));
}

/// There is nothing else in the month, and the money still shows.
#[tokio::test]
async fn a_month_of_only_left_out_money_has_a_row() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    let removed = world.posted(checking, "2026-10-07", -500, food).await;
    world.plant(REMOVED, &removed).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(0));
    assert_eq!(row["left_out"], usd(500));
}

/// As in the forecast: whether it was real is unknown, and it keeps counting.
#[tokio::test]
async fn a_flagged_refund_keeps_counting() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world.posted(checking, "2026-10-06", -500, food).await;
    for (flag, on) in [(REMOVED, "2026-10-07"), (REVISED, "2026-10-08")] {
        let refund = world.posted(checking, on, 30, food).await;
        world.plant(flag, &refund).await;
    }

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(440));
    assert_eq!(row["left_out"], usd(0));
}

/// In an `income` category the money that adds to `actual` is a deposit, so that is what
/// a removal or a revision takes out.
#[tokio::test]
async fn a_flagged_deposit_is_left_out_of_income_and_reported() {
    let world = world().await;
    let checking = world.checking().await;
    let pay = world.category("Pay", "income").await;
    world.posted(checking, "2026-10-06", 30, pay).await;
    for (flag, on) in [(REMOVED, "2026-10-07"), (REVISED, "2026-10-08")] {
        let deposit = world.posted(checking, on, 500, pay).await;
        world.plant(flag, &deposit).await;
    }

    let report = world.october().await;

    let row = row(&report, "2026-10", pay);
    assert_eq!(row["actual"], usd(30));
    assert_eq!(row["left_out"], usd(1_000));
}

#[tokio::test]
async fn a_flagged_clawback_keeps_lowering_income() {
    let world = world().await;
    let checking = world.checking().await;
    let pay = world.category("Pay", "income").await;
    world.posted(checking, "2026-10-06", 500, pay).await;
    let clawback = world.posted(checking, "2026-10-07", -30, pay).await;
    world.plant(REMOVED, &clawback).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", pay);
    assert_eq!(row["actual"], usd(470));
    assert_eq!(row["left_out"], usd(0));
}

#[tokio::test]
async fn a_possible_duplicate_counts_and_is_reported() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world.posted(checking, "2026-10-06", -30, food).await;
    let duplicate = world.posted(checking, "2026-10-06", -500, food).await;
    world.plant(DUPLICATE, &duplicate).await;
    let refund = world.posted(checking, "2026-10-07", 7, food).await;
    world.plant(DUPLICATE, &refund).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(523));
    assert_eq!(row["possible_duplicate"], usd(500), "the charge alone");
    assert_eq!(row["left_out"], usd(0));
}

/// Left out is left out: it isn't a duplicate within `actual` as well.
#[tokio::test]
async fn a_removed_possible_duplicate_is_only_left_out() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    let both = world.posted(checking, "2026-10-06", -500, food).await;
    world.plant(DUPLICATE, &both).await;
    world.plant(REMOVED, &both).await;

    let report = world.october().await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["left_out"], usd(500));
    assert_eq!(row["possible_duplicate"], usd(0));
}

/// A charge that doesn't say what was spent leaves the budget unspent, as in the forecast.
#[tokio::test]
async fn a_left_out_charge_does_not_spend_a_budget() {
    let world = world().await;
    let (checking, food) = world.food_on_checking().await;
    world.item(&budget("Groceries", 1_000, food)).await;
    let removed = world.posted(checking, "2026-10-07", -500, food).await;
    world.plant(REMOVED, &removed).await;

    let report = world.october().await;

    assert_eq!(
        report["rollovers"][0]["rollover"],
        usd(1_000),
        "the row says why"
    );
    assert_eq!(row(&report, "2026-10", food)["left_out"], usd(500));
}

/// Nothing has claimed an uncategorized charge, so a flag changes nothing about it: the
/// forecast's loose ends count it the same way.
#[tokio::test]
async fn a_removed_uncategorized_charge_stays_uncategorized() {
    let world = world().await;
    let checking = world.checking().await;
    let removed = world.manual(checking, "2026-10-07", -500).await;
    world.plant(REMOVED, &removed).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "still no category");
    assert_eq!(
        report["uncategorized"],
        json!([{"month": "2026-10", "currency": "USD", "count": 1, "total": usd(500)}])
    );
}
