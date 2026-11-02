//! Money that isn't the household's, through the router and the store's real queries
//! (spec §8.11 B8, B9, B10): a held account moves no number, and a contribution to one is
//! money leaving, held once and never twice.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::answer::{Golden, golden};
use super::settled::holding;
use super::unknowns::{fed, reported};
use super::{
    AS_OF, BALANCE_ON, ITEMS, World, caution, goal_schedule, kinds, line, monthly, noon,
    one_off_on, outflow, usd, with, world,
};

/// The day of the contribution examples, the 5th's bill having passed.
const THE_10TH: &str = "2026-11-10";
/// A balance from before the 5th's charge.
const BEFORE_THE_CHARGE: &str = "2026-11-04";
/// A balance from after it.
const AFTER_THE_CHARGE: &str = "2026-11-09";
const ONE_MONTH: &str = "?horizon_months=1";

/// `forecast`'s line for what is called `name`.
fn listed<'a>(forecast: &'a Value, name: &str) -> &'a Value {
    forecast["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap()
}

/// `forecast`'s only totals.
fn totals(forecast: &Value) -> &Value {
    let [only] = forecast["totals"].as_array().unwrap().as_slice() else {
        panic!("expected one currency");
    };
    only
}

impl World {
    /// A savings account in USD, class `cash` as its kind has it, held for someone else.
    async fn held_savings(&self, name: &str) -> AccountId {
        self.account_with(
            name,
            "savings",
            "USD",
            json!({"belongs_to": "someone_else"}),
        )
        .await
    }

    /// A 529 in USD, by its kind held for someone else and invested.
    async fn college_fund(&self) -> AccountId {
        self.account_with("College fund", "education", "USD", json!({}))
            .await
    }
}

/// On the 10th: Checking at 3,000.00 from before the 5th, and a floor of 0.
async fn on_the_10th() -> (World, AccountId) {
    let world = world().await;
    world.server.clock.set(noon(THE_10TH));
    let checking = world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.balance(checking, BEFORE_THE_CHARGE, 300_000).await;
    world.floor(0).await;
    (world, checking)
}

/// A monthly bill of 500.00 on the 5th, hinted at `checking`. With `end_on`, November's is
/// its last, so one occurrence is all there is to count; without, December's is in any
/// window, since the shortest ends on the 10th.
async fn contribution_bill(world: &World, checking: AccountId, end_on: Option<&str>) -> CategoryId {
    let category = world.expense("College").await;
    let mut schedule = monthly("2026-01-05");
    if let Some(end_on) = end_on {
        schedule["end_on"] = json!(end_on);
    }
    world
        .item(&with(
            outflow("College fund contribution", 50_000, category, &schedule),
            "account_hint",
            json!(checking),
        ))
        .await;
    category
}

#[tokio::test]
async fn a_custodial_account_doesn_t_move_the_number() {
    let (world, _) = holding().await;
    assert_eq!(world.forecast().await["surplus"], usd(1_000_000));

    let fund = world
        .account_with("For a child", "custodial", "USD", json!({}))
        .await;
    world.balance(fund, BALANCE_ON, 9_000_000).await;
    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(1_000_000));
    assert_eq!(forecast["status"], "firm");
    let held = listed(&forecast, "For a child");
    assert_eq!(held["counted"], false);
    assert_eq!(held["not_counted"], "not_household");
}

/// Its 5,000.00 isn't cash, its 100.00 on food reduces no budget and its 70.00 with no
/// category raises nothing: 10,000.00 less the budget's 600.00 is 9,400.00, firm.
#[tokio::test]
async fn a_held_cash_account_stays_out() {
    let (world, _) = holding().await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 60_000, food, &monthly("2026-01-28")),
            "spending",
            json!("budget"),
        ))
        .await;
    let held = world.held_savings("Held savings").await;
    world.balance(held, BALANCE_ON, 500_000).await;
    world.spend(held, "2026-11-02", 10_000, food).await;
    world.manual(held, "2026-11-03", -7_000).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        listed(&forecast, "Held savings")["not_counted"],
        "not_household"
    );
    assert_eq!(totals(&forecast)["cash_available"], usd(1_000_000));
    assert_eq!(line(&forecast, "Groceries")["reduced_by"], usd(0));
    assert_eq!(forecast["surplus"], usd(940_000));
    assert_eq!(forecast["status"], "firm");
    assert_eq!(forecast["cautions"], json!([]));
}

#[tokio::test]
async fn a_held_cash_account_with_no_balance_blocks_nothing() {
    let (world, _) = holding().await;
    world.held_savings("Held savings").await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["blockers"], json!([]));
    assert_eq!(forecast["surplus"], usd(1_000_000));
}

#[tokio::test]
async fn only_held_money_is_no_money_to_forecast_from() {
    let world = world().await;
    world.floor(0).await;
    let held = world.held_savings("Held savings").await;
    world.balance(held, BALANCE_ON, 500_000).await;

    let forecast = world.forecast().await;

    assert_eq!(kinds(&forecast, "blockers"), ["no_counted_accounts"]);
    assert_eq!(forecast["surplus"], Value::Null);
}

#[tokio::test]
async fn a_bucket_in_another_currency_raises_no_mixed_currencies() {
    let (world, _) = holding().await;
    let fund = world
        .account_with(
            "In euros",
            "savings",
            "EUR",
            json!({"belongs_to": "given_away"}),
        )
        .await;
    world.balance(fund, BALANCE_ON, 500_000).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["blockers"], json!([]));
    assert_eq!(totals(&forecast)["currency"], "USD");
    assert_eq!(forecast["surplus"], usd(1_000_000));
}

#[tokio::test]
async fn binding_a_529_lifts_the_unbound_caution() {
    let fed = fed().await;
    fed.sync(reported("USD")).await;
    let before = fed.world.forecast().await;
    assert_eq!(kinds(&before, "cautions"), ["unbound_provider_account"]);
    assert_eq!(before["surplus"], usd(1_000));

    let fund = fed.world.college_fund().await;
    fed.bound(fund).await;
    let after = fed.world.forecast().await;

    assert_eq!(after["cautions"], json!([]));
    assert_eq!(after["status"], "firm");
    assert_eq!(after["surplus"], usd(1_000));
}

/// The hand check, one month from the 10th, with November's the bill's last:
///
/// - No such bill: 3,000.00 of cash and nothing needed, a surplus of 3,000.00.
/// - Before the charge: 3,000.00, and the 5th has passed with nothing spent, so a gap of
///   500.00 is needed: 2,500.00, with a caution.
/// - After it: the balance shows it, at 2,500.00, and the 500.00 spent pays the bill, so
///   nothing is needed: 2,500.00, firm.
#[tokio::test]
async fn a_contribution_is_money_leaving() {
    let (without, _) = on_the_10th().await;
    assert_eq!(
        without.forecast_at(ONE_MONTH).await["surplus"],
        usd(300_000)
    );

    let (world, checking) = on_the_10th().await;
    let category = contribution_bill(&world, checking, Some("2026-11-30")).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["cash_available"], usd(300_000));
    assert_eq!(
        line(&before, "College fund contribution")["unpaid"],
        usd(50_000)
    );
    assert_eq!(before["surplus"], usd(250_000));
    assert_eq!(caution(&before, "bill_not_seen")["amount"], usd(50_000));

    world.spend(checking, "2026-11-05", 50_000, category).await;
    world.balance(checking, AFTER_THE_CHARGE, 250_000).await;

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&after)["cash_available"], usd(250_000));
    assert_eq!(totals(&after)["planned_outflows"], usd(0));
    assert_eq!(after["surplus"], usd(250_000));
    assert_eq!(after["status"], "firm");
}

/// The same, for a bill with no end: December's 500.00 counts on both sides, so 3,000.00
/// less 1,000.00 before the charge and 2,500.00 less 500.00 after it are both 2,000.00.
#[tokio::test]
async fn a_contribution_still_to_come_counts_too() {
    let (world, checking) = on_the_10th().await;
    let category = contribution_bill(&world, checking, None).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["planned_outflows"], usd(100_000));
    assert_eq!(before["surplus"], usd(200_000));

    world.spend(checking, "2026-11-05", 50_000, category).await;
    world.balance(checking, AFTER_THE_CHARGE, 250_000).await;

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&after)["planned_outflows"], usd(50_000));
    assert_eq!(after["surplus"], usd(200_000));
}

/// Filed as a `transfer`, it pays no bill: the balance falls to 2,500.00 and the 500.00 gap
/// stays, so the surplus is 2,000.00, not 2,500.00. That holds 500.00 more than it need,
/// which is the safe direction, and the caution says why.
#[tokio::test]
async fn a_contribution_in_a_transfer_category_stays_unpaid() {
    let (world, checking) = on_the_10th().await;
    contribution_bill(&world, checking, Some("2026-11-30")).await;
    let moved = world.category("Moved", "transfer").await;

    world.spend(checking, "2026-11-05", 50_000, moved).await;
    world.balance(checking, AFTER_THE_CHARGE, 250_000).await;

    let forecast = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&forecast)["cash_available"], usd(250_000));
    assert_eq!(
        line(&forecast, "College fund contribution")["unpaid"],
        usd(50_000)
    );
    assert_eq!(forecast["surplus"], usd(200_000));
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(caution(&forecast, "bill_not_seen")["amount"], usd(50_000));
}

/// Filed in the bill's own category with the balance not showing it yet: cash stays
/// 3,000.00, so the 500.00 can't also count as paid, or the surplus would read 3,000.00 for
/// money already gone. It stays 2,500.00, and two cautions say why.
#[tokio::test]
async fn a_contribution_the_balance_doesn_t_show_pays_nothing_yet() {
    let (world, checking) = on_the_10th().await;
    let category = contribution_bill(&world, checking, Some("2026-11-30")).await;

    world.spend(checking, "2026-11-05", 50_000, category).await;

    let forecast = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&forecast)["cash_available"], usd(300_000));
    assert_eq!(forecast["surplus"], usd(250_000));
    assert_eq!(
        kinds(&forecast, "cautions"),
        ["bill_not_seen", "spending_newer_than_balance"]
    );
}

/// Spending pays a bill only in an `expense` category. A bill can't be planned in a
/// `transfer` one, so its category is planted as having become one, the one way a transfer
/// could meet a bill: the balance falls to 2,500.00 and the gap stays, so 2,000.00.
#[tokio::test]
async fn a_charge_in_a_transfer_category_never_pays_a_bill() {
    let (world, checking) = on_the_10th().await;
    let category = contribution_bill(&world, checking, Some("2026-11-30")).await;
    world.spend(checking, "2026-11-05", 50_000, category).await;
    world.balance(checking, AFTER_THE_CHARGE, 250_000).await;
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE categories SET kind = 'transfer' WHERE id = ?",
            &category.to_string(),
        )
        .await
        .unwrap();

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(forecast["surplus"], usd(200_000));
    assert_eq!(forecast["status"], "provisional");
}

#[tokio::test]
async fn a_planned_transfer_is_refused() {
    let world = world().await;
    let moved = world.category("Moved", "transfer").await;

    let (status, _) = world
        .send(
            Method::POST,
            ITEMS,
            &world.admin,
            Some(&outflow(
                "College fund contribution",
                50_000,
                moved,
                &monthly("2026-01-05"),
            )),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// 600.00 of goal due in the window is 100.00 less once 100.00 is linked to it, though that
/// charge is newer than the fund's own balance: it never touched counted cash.
#[tokio::test]
async fn a_goal_paid_from_a_529_lowers_the_need() {
    let (world, _) = holding().await;
    let tuition = world.expense("Tuition").await;
    let goal = world
        .item(&outflow(
            "First year",
            60_000,
            tuition,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    assert_eq!(world.forecast().await["surplus"], usd(940_000));
    let fund = world.college_fund().await;
    world.balance(fund, BALANCE_ON, 200_000).await;

    world
        .spend_linked(fund, AS_OF, 10_000, tuition, &goal)
        .await;

    let forecast = world.forecast().await;
    assert_eq!(line(&forecast, "First year")["counted"], usd(50_000));
    assert_eq!(forecast["surplus"], usd(950_000));
    assert_eq!(forecast["status"], "firm");
}

/// One month from the 15th: December's 2,000.00 and November's, which the payment doesn't
/// pay, since it left no counted cash: 10,000.00 - 4,000.00. Settled, November's goes:
/// 10,000.00 - 2,000.00. Of class `cash`, with a balance dated after the charge, so that only
/// whose money it is keeps the payment from the bill.
#[tokio::test]
async fn a_bill_paid_from_a_bucket_stays_unpaid_until_settled() {
    let (world, _) = holding().await;
    let tuition = world.expense("Tuition").await;
    let bill = world
        .item(&outflow("Term", 200_000, tuition, &monthly("2026-01-01")))
        .await;
    let held = world.held_savings("Held savings").await;
    world.balance(held, BALANCE_ON, 500_000).await;
    world.spend(held, "2026-11-02", 200_000, tuition).await;

    let unpaid = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&unpaid, "Term")["unpaid"], usd(200_000));
    assert_eq!(unpaid["surplus"], usd(600_000));
    assert_eq!(kinds(&unpaid, "cautions"), ["bill_not_seen"]);

    world.settle(&bill, "2026-11", "paid").await;

    let settled = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&settled, "Term")["unpaid"], usd(0));
    assert_eq!(settled["surplus"], usd(800_000));
    assert_eq!(settled["status"], "firm");
}

#[tokio::test]
async fn a_hinted_contribution_still_counts_with_a_caution() {
    let (world, _) = holding().await;
    let fund = world
        .account_with("For a child", "custodial", "USD", json!({}))
        .await;
    let gifts = world.expense("Gifts").await;
    world
        .item(&with(
            outflow("Birthday", 30_000, gifts, &one_off_on("2026-12-01")),
            "account_hint",
            json!(fund),
        ))
        .await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(forecast["surplus"], usd(970_000));
    assert_eq!(forecast["status"], "provisional");
    assert!(
        caution(&forecast, "item_hint_not_counted")["account"] == json!(fund),
        "names the fund"
    );
}

/// The worked example, beside a 529, a custodial account in euros and a fund with money in
/// it, and a utility bill paid from the 529: every figure as `answer.rs` works it out, and
/// the water bill still unseen.
#[tokio::test]
async fn the_worked_example_is_the_same_beside_held_accounts() {
    let Golden {
        world, utilities, ..
    } = golden().await;
    let college = world.college_fund().await;
    world.balance(college, BALANCE_ON, 9_000_000).await;
    world.spend(college, "2026-11-10", 12_000, utilities).await;
    let euros = world
        .account_with("In euros", "custodial", "EUR", json!({}))
        .await;
    world.balance(euros, BALANCE_ON, 4_000_000).await;
    let given = world
        .account_with("Giving fund", "charitable", "USD", json!({}))
        .await;
    world.balance(given, BALANCE_ON, 2_000_000).await;

    let forecast = world.forecast().await;

    let row = totals(&forecast);
    assert_eq!(row["cash_available"], usd(6_850_000));
    assert_eq!(row["planned_outflows"], usd(914_000));
    assert_eq!(row["goals"], usd(1_100_000));
    assert_eq!(row["floor"], usd(1_000_000));
    assert_eq!(row["cash_needed"], usd(3_014_000));
    assert_eq!(forecast["surplus"], usd(3_836_000));
    assert_eq!(kinds(&forecast, "cautions"), ["bill_not_seen"]);
}

/// A Checking with no balance yet, a floor of 0, and a goal of 1,000.00 due in the window:
/// the world, the Checking, the goal's category and its id.
async fn saving_for_a_goal() -> (World, AccountId, CategoryId, Value) {
    let world = world().await;
    let checking = world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.floor(0).await;
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

/// A gift of 1,000.00 expected in the 529, as `include_income` would otherwise count it.
async fn gift_into_the_fund(world: &World) -> AccountId {
    let fund = world.college_fund().await;
    let gifts = world.category("Gifts", "income").await;
    world
        .item(&json!({
            "name": "Gift",
            "category": gifts,
            "amount": usd(100_000),
            "direction": "inflow",
            "account_hint": fund,
            "schedule": one_off_on("2026-12-01"),
        }))
        .await;
    fund
}

/// 10,000.00 and nothing more: the gift isn't the household's to invest.
#[tokio::test]
async fn an_inflow_hinted_at_a_held_account_stays_out() {
    let (world, _) = holding().await;
    let fund = gift_into_the_fund(&world).await;

    let forecast = world
        .forecast_at("?horizon_months=1&include_income=true")
        .await;

    assert_eq!(forecast["surplus"], usd(1_000_000));
    assert_eq!(totals(&forecast)["planned_inflows"], usd(0));
    assert_eq!(line(&forecast, "Gift")["counted"], usd(0));
    assert_eq!(forecast["months"][0]["lines"][0]["inflows"], usd(0));
    assert_eq!(forecast["status"], "provisional");
    let said = caution(&forecast, "item_hint_not_counted");
    assert!(said["account"] == json!(fund), "names the fund");
    assert_eq!(said["item"]["id"], line(&forecast, "Gift")["source"]["id"]);
    assert_eq!(said["effect"], "left_out");
}

/// With income off it is not "income left out": turning income on wouldn't add it. Nothing
/// about the number depends on it, so it stays firm.
#[tokio::test]
async fn an_inflow_hinted_at_a_held_account_is_not_excluded_income() {
    let (world, _) = holding().await;
    gift_into_the_fund(&world).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(totals(&forecast)["excluded_inflows"], usd(0));
    assert_eq!(forecast["surplus"], usd(1_000_000));
    assert_eq!(forecast["status"], "firm");
}
