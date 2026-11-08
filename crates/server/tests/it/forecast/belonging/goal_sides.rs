//! A goal with spending linked on both the household's accounts and a held one (spec §8.11
//! B10): the forecast can't tell a contribution from the payment it funded, so it says so,
//! and only charges that count are sides.

use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::super::{AS_OF, BALANCE_ON, World, caution, goal_schedule, kinds, outflow, usd, world};
use super::totals;

/// A Checking with no balance yet, an emergency fund of 0, and a goal of 1,000.00 due in the window:
/// the world, the Checking, the goal's category and its id.
async fn saving_for_a_goal() -> (World, AccountId, CategoryId, Value) {
    let world = world().await;
    let checking = world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.emergency_fund(0).await;
    let category = world.expense("College").await;
    let goal = world
        .item(&outflow(
            "First year",
            100_000,
            category,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    (world, checking, category, goal)
}

/// The same 500.00 may be linked twice, as the contribution and as the tuition it paid:
/// nothing is left of the need, so 2,500.00 with no goal to hold. The data can't say whether
/// that is right, so the number stays and stops being firm.
#[tokio::test]
async fn a_goal_linked_on_both_sides_says_so() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world
        .spend_linked(checking, "2026-11-02", 50_000, category, &goal)
        .await;
    world.balance(checking, BALANCE_ON, 250_000).await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;

    let forecast = world.forecast().await;

    assert_eq!(totals(&forecast)["goals"], usd(0));
    assert_eq!(forecast["surplus"], usd(250_000));
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(kinds(&forecast, "cautions"), ["goal_linked_both_sides"]);
    assert_eq!(
        caution(&forecast, "goal_linked_both_sides")["goal"],
        json!({"kind": "planned_item", "id": goal})
    );
}

/// The contribution alone: 2,500.00 less the 500.00 the goal still needs.
#[tokio::test]
async fn a_goal_linked_to_the_contribution_alone_raises_nothing() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    world.college_fund().await;
    world
        .spend_linked(checking, "2026-11-02", 50_000, category, &goal)
        .await;
    world.balance(checking, BALANCE_ON, 250_000).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(200_000));
    assert_eq!(forecast["cautions"], json!([]));
    assert_eq!(forecast["status"], "firm");
}

/// The tuition alone: the same 500.00 is still in Checking at 3,000.00, and the goal needs
/// 500.00 more.
#[tokio::test]
async fn a_goal_linked_to_the_tuition_alone_raises_nothing() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(250_000));
    assert_eq!(forecast["cautions"], json!([]));
    assert_eq!(forecast["status"], "firm");
}

/// 500.00 linked on `side` and 500.00 of tuition on the 529, with Checking at 3,000.00.
async fn link_both_sides(
    world: &World,
    checking: AccountId,
    side: AccountId,
    category: CategoryId,
    goal: &Value,
) {
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(side, "2026-11-02", 50_000, category, goal)
        .await;
    let fund = world.college_fund().await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, goal)
        .await;
}

/// The need falls twice whether or not the household's side touched counted cash, so nothing
/// is left of it and Checking's 3,000.00 is all surplus, which only the caution questions.
fn assert_goal_questioned(forecast: &Value) {
    assert_eq!(totals(forecast)["goals"], usd(0));
    assert_eq!(forecast["surplus"], usd(300_000));
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(kinds(forecast, "cautions"), ["goal_linked_both_sides"]);
}

#[tokio::test]
async fn a_goal_linked_on_a_switched_out_account_and_a_held_one_says_so() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let side = world
        .account_with(
            "Side",
            "checking",
            "USD",
            json!({"include_in_forecast": false}),
        )
        .await;

    link_both_sides(&world, checking, side, category, &goal).await;

    assert_goal_questioned(&world.forecast().await);
}

#[tokio::test]
async fn a_goal_linked_on_a_brokerage_and_a_held_one_says_so() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let side = world
        .account_with("Brokerage", "brokerage", "USD", json!({}))
        .await;

    link_both_sides(&world, checking, side, category, &goal).await;

    assert_goal_questioned(&world.forecast().await);
}

#[tokio::test]
async fn a_goal_linked_on_a_closed_account_and_a_held_one_says_so() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let side = world
        .account_with("Old", "checking", "USD", json!({}))
        .await;

    link_both_sides(&world, checking, side, category, &goal).await;
    world.close(side).await;

    assert_goal_questioned(&world.forecast().await);
}

/// One held account funding another: the forecast can't tell, which the docs say.
#[tokio::test]
async fn a_goal_linked_on_two_held_accounts_raises_nothing() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    let fund = world.college_fund().await;
    let other = world
        .account_with("For a child", "custodial", "USD", json!({}))
        .await;
    world
        .spend_linked(fund, "2026-11-02", 10_000, category, &goal)
        .await;
    world
        .spend_linked(other, "2026-11-03", 10_000, category, &goal)
        .await;

    assert_eq!(world.forecast().await["cautions"], json!([]));
}

/// A charge is a side even after its refund: removing the link is what clears the caution.
#[tokio::test]
async fn a_refunded_charge_is_still_a_side() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    let fund = world.college_fund().await;
    world
        .spend_linked(checking, "2026-11-02", 50_000, category, &goal)
        .await;
    world
        .spend_linked(checking, "2026-11-03", -50_000, category, &goal)
        .await;
    world
        .spend_linked(fund, "2026-11-04", 50_000, category, &goal)
        .await;

    assert_eq!(
        kinds(&world.forecast().await, "cautions"),
        ["goal_linked_both_sides"]
    );
}

/// Linked on Checking and newer than its balance, it hasn't lowered the need yet, but it
/// will on the next balance, so it says so already.
#[tokio::test]
async fn a_goal_link_newer_than_the_balance_is_a_side_too() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(checking, AS_OF, 50_000, category, &goal)
        .await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;

    let forecast = world.forecast().await;

    assert!(
        kinds(&forecast, "cautions").contains(&"goal_linked_both_sides".to_owned()),
        "both sides are linked"
    );
}

/// A charge the provider took back says nothing about what was spent (R10a), so it is no side.
#[tokio::test]
async fn a_removed_charge_is_no_side_of_a_goal() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    let removed = world
        .spend_linked(checking, "2026-11-02", 50_000, category, &goal)
        .await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE transactions SET flag_source_removed = 1 WHERE id = ?",
            removed.as_str().unwrap(),
        )
        .await
        .unwrap();
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;

    assert_eq!(world.forecast().await["cautions"], json!([]));
}

/// Money back on Checking lowers nothing from it, so it is no side.
#[tokio::test]
async fn a_refund_is_no_side_of_a_goal() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(checking, "2026-11-02", -5_000, category, &goal)
        .await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;

    assert_eq!(world.forecast().await["cautions"], json!([]));
}

#[tokio::test]
async fn a_pending_charge_is_no_side_of_a_goal() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;
    let pending = world
        .spend_linked(checking, "2026-11-02", 50_000, category, &goal)
        .await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE transactions SET pending = 1 WHERE id = ?",
            pending.as_str().unwrap(),
        )
        .await
        .unwrap();

    let forecast = world.forecast().await;

    assert!(
        !kinds(&forecast, "cautions").contains(&"goal_linked_both_sides".to_owned()),
        "not posted yet"
    );
}

/// What the forecast's day hasn't reached yet isn't a side of it.
#[tokio::test]
async fn a_charge_after_as_of_is_no_side_of_a_goal() {
    let (world, checking, category, goal) = saving_for_a_goal().await;
    let fund = world.college_fund().await;
    world.balance(checking, BALANCE_ON, 300_000).await;
    world
        .spend_linked(fund, "2026-11-03", 50_000, category, &goal)
        .await;
    world
        .spend_linked(checking, "2026-12-01", 50_000, category, &goal)
        .await;

    assert_eq!(world.forecast().await["cautions"], json!([]));
}
