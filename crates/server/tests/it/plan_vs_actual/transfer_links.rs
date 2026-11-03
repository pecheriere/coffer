//! Linked transfers in the report (spec §8.12 T5): one movement between counted accounts is
//! neither spending nor income, and says so beside the rows. Any other link changes nothing.

use axum::http::Method;
use coffer_domain::ids::AccountId;
use serde_json::{Value, json};

use super::{World, budget, goal, is_empty, row, usd, world};

const OCTOBER: &str = "2026-10";

impl World {
    async fn savings(&self) -> AccountId {
        self.holding("Savings", "savings").await
    }

    async fn linked(&self, outflow: &str, inflow: &str) -> Value {
        self.admin_send(
            Method::POST,
            "/api/v1/transfer-links",
            &json!({"outflow": outflow, "inflow": inflow}),
        )
        .await
    }

    async fn unlinked(&self, link: &Value) {
        let (status, _) = self
            .send(
                Method::DELETE,
                &format!("/api/v1/transfer-links/{}", link["id"].as_str().unwrap()),
                &self.admin,
                None,
            )
            .await;
        assert!(status.is_success());
    }
}

/// What `moved_between_counted_accounts` says of October in USD, and nothing else.
fn moved(out: i64, into: i64) -> Value {
    json!([{"month": OCTOBER, "money_out": usd(out), "money_in": usd(into)}])
}

/// Filed by hand as a purchase and as pay, 500.00 is spending and income until it is linked;
/// then it is one movement, in neither, and said once. Removing the link brings it back.
#[tokio::test]
async fn a_link_takes_both_legs_out_of_actual() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let food = world.expense("Food").await;
    let pay = world.category("Pay", "income").await;
    world.posted(checking, "2026-10-02", -3_000, food).await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, food).await;
    let inflow = world.posted(savings, "2026-10-05", 50_000, pay).await;

    let before = world.october().await;
    assert_eq!(row(&before, OCTOBER, food)["actual"], usd(53_000));
    assert_eq!(row(&before, OCTOBER, pay)["actual"], usd(50_000));
    assert!(is_empty(&before, "moved_between_counted_accounts"));

    let link = world.linked(&outflow, &inflow).await;

    let after = world.october().await;
    assert_eq!(row(&after, OCTOBER, food)["actual"], usd(3_000));
    let rows = after["rows"].as_array().unwrap();
    assert!(
        rows.iter().all(|row| row["category"] != json!(pay)),
        "pay has nothing left to say"
    );
    assert_eq!(
        after["moved_between_counted_accounts"],
        moved(50_000, 50_000)
    );

    world.unlinked(&link).await;
    assert_eq!(world.october().await, before);
}

/// A budget's rollover reads `actual`, so a linked leg doesn't eat it.
#[tokio::test]
async fn a_linked_leg_spends_no_budget() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let food = world.expense("Food").await;
    world.item(&budget("Groceries", 40_000, food)).await;
    let outflow = world.posted(checking, "2026-10-05", -30_000, food).await;
    let inflow = world.manual(savings, "2026-10-05", 30_000).await;

    let report = world.october().await;
    assert_eq!(report["rollovers"][0]["spent"], usd(30_000));

    world.linked(&outflow, &inflow).await;

    let report = world.october().await;
    assert_eq!(report["rollovers"][0]["spent"], usd(0));
    assert_eq!(report["rollovers"][0]["rollover"], usd(40_000));
}

/// An uncategorized leg isn't uncategorized spending once a person has said where it went.
#[tokio::test]
async fn a_linked_leg_isnt_uncategorized() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let outflow = world.manual(checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(savings, "2026-10-05", 50_000).await;
    world.manual(checking, "2026-10-06", -1_000).await;

    let before = world.october().await;
    assert_eq!(before["uncategorized"][0]["count"], 2);
    assert_eq!(before["uncategorized"][0]["total"], usd(51_000));

    world.linked(&outflow, &inflow).await;

    let after = world.october().await;
    assert_eq!(after["uncategorized"][0]["count"], 1);
    assert_eq!(after["uncategorized"][0]["total"], usd(1_000));
    assert_eq!(
        after["moved_between_counted_accounts"],
        moved(50_000, 50_000)
    );
}

/// Filed as `transfer`, it was in no row already: the link says where it went, nothing more.
#[tokio::test]
async fn a_linked_leg_in_a_transfer_category_changes_no_row() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let category = world.category("Moved", "transfer").await;
    let outflow = world
        .posted(checking, "2026-10-05", -50_000, category)
        .await;
    let inflow = world.posted(savings, "2026-10-05", 50_000, category).await;
    let before = world.october().await;

    world.linked(&outflow, &inflow).await;

    let after = world.october().await;
    assert_eq!(after["rows"], before["rows"]);
    assert_eq!(
        after["moved_between_counted_accounts"],
        moved(50_000, 50_000)
    );
}

/// Only what moved leaves: 500.00 left and 499.00 arrived, so the 1.00 is what it is filed as.
#[tokio::test]
async fn a_difference_stays_what_it_is_filed_as() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let fees = world.expense("Fees").await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, fees).await;
    let inflow = world.manual(savings, "2026-10-05", 49_900).await;

    world.linked(&outflow, &inflow).await;

    let report = world.october().await;
    assert_eq!(row(&report, OCTOBER, fees)["actual"], usd(100));
    assert_eq!(
        report["moved_between_counted_accounts"],
        moved(49_900, 49_900)
    );
}

/// A wrong link, or a provider's revision, can't make real spending disappear: linked to 12.50,
/// a charge of 1,250.00 still spends 1,237.50, categorized or not.
#[tokio::test]
async fn nothing_but_what_moved_leaves_the_counts() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let food = world.expense("Food").await;
    let filed = world.posted(checking, "2026-10-05", -125_000, food).await;
    let into = world.manual(savings, "2026-10-05", 1_250).await;
    world.linked(&filed, &into).await;
    let unfiled = world.manual(checking, "2026-10-06", -125_000).await;
    let into = world.manual(savings, "2026-10-06", 1_250).await;
    world.linked(&unfiled, &into).await;

    let report = world.october().await;

    assert_eq!(row(&report, OCTOBER, food)["actual"], usd(123_750));
    assert_eq!(report["uncategorized"][0]["count"], 1);
    assert_eq!(report["uncategorized"][0]["total"], usd(123_750));
    assert_eq!(
        report["moved_between_counted_accounts"],
        moved(2_500, 2_500)
    );
}

/// A leg its provider took back ends the link's say over the numbers. -800.00 filed as Food and
/// +800.00 as Pay: without the deposit, the charge is spending again and nothing says 800.00
/// moved. Without the charge, it is left out, and says how much.
#[tokio::test]
async fn a_leg_taken_back_brings_the_money_back() {
    for (taken_back_out, food_actual, food_left_out, pay_actual, pay_left_out) in
        [(false, 80_000, 0, 0, 80_000), (true, 0, 80_000, 80_000, 0)]
    {
        let world = world().await;
        let checking = world.checking().await;
        let savings = world.savings().await;
        let food = world.expense("Food").await;
        let pay = world.category("Pay", "income").await;
        let outflow = world.posted(checking, "2026-10-05", -80_000, food).await;
        let inflow = world.posted(savings, "2026-10-05", 80_000, pay).await;
        world.linked(&outflow, &inflow).await;
        let taken_back = if taken_back_out { &outflow } else { &inflow };
        world.plant(super::REMOVED, taken_back).await;

        let report = world.october().await;

        assert!(is_empty(&report, "moved_between_counted_accounts"));
        assert_eq!(row(&report, OCTOBER, food)["actual"], usd(food_actual));
        assert_eq!(row(&report, OCTOBER, food)["left_out"], usd(food_left_out));
        assert_eq!(row(&report, OCTOBER, pay)["actual"], usd(pay_actual));
        assert_eq!(row(&report, OCTOBER, pay)["left_out"], usd(pay_left_out));
    }
}

/// Two legs a provider has revised to one sign aren't a movement, whatever the link says.
#[tokio::test]
async fn two_legs_of_one_sign_move_nothing() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let food = world.expense("Food").await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, food).await;
    let inflow = world.manual(savings, "2026-10-05", 50_000).await;
    world.linked(&outflow, &inflow).await;
    world
        .plant(
            "UPDATE transactions SET amount_minor = -50000 WHERE id = ?",
            &inflow,
        )
        .await;

    let report = world.october().await;

    assert_eq!(row(&report, OCTOBER, food)["actual"], usd(50_000));
    assert!(is_empty(&report, "moved_between_counted_accounts"));
}

/// Each currency has its own line, and a leg posted in another month is that month's.
#[tokio::test]
async fn each_side_is_said_in_its_own_month_and_currency() {
    let world = world().await;
    let checking = world.checking().await;
    let euros = world.holding_in("Euros", "savings", "EUR").await;
    let outflow = world.manual(checking, "2026-10-31", -50_000).await;
    let inflow = world.manual(euros, "2026-11-02", 46_000).await;

    world.linked(&outflow, &inflow).await;

    let report = world.report("from=2026-10&months=2&average_months=1").await;
    assert_eq!(
        report["moved_between_counted_accounts"],
        json!([
            {"month": "2026-10", "money_out": usd(50_000), "money_in": usd(0)},
            {
                "month": "2026-11",
                "money_out": {"minor_units": 0, "currency": "EUR"},
                "money_in": {"minor_units": 46_000, "currency": "EUR"},
            },
        ])
    );
}

/// A month read only for a rolling average isn't one the report answers for.
#[tokio::test]
async fn only_the_reported_months_are_said() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let outflow = world.manual(checking, "2026-09-05", -50_000).await;
    let inflow = world.manual(savings, "2026-09-05", 50_000).await;
    world.linked(&outflow, &inflow).await;

    let report = world.report("from=2026-10&months=1&average_months=3").await;

    assert!(is_empty(&report, "moved_between_counted_accounts"));
}

/// A pending leg is in no report yet, linked or not.
#[tokio::test]
async fn a_pending_leg_says_nothing_yet() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let outflow = world.manual(checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(savings, "2026-10-05", 50_000).await;
    world.linked(&outflow, &inflow).await;
    world.plant(super::PENDING, &outflow).await;

    assert_eq!(
        world.october().await["moved_between_counted_accounts"],
        moved(0, 50_000)
    );
}

/// Spec §8.11 B9: money leaving counted cash is spending. The link records the movement and
/// every number stays what it was, on both sides.
#[tokio::test]
async fn a_link_out_of_the_household_changes_no_number() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("College fund", "education").await;
    let college = world.expense("College").await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, college).await;
    let inflow = world.posted(fund, "2026-10-05", 50_000, college).await;
    let before = world.october().await;
    assert_eq!(row(&before, OCTOBER, college)["actual"], usd(50_000));
    assert_eq!(before["not_the_households"][0]["net"], usd(50_000));

    let link = world.linked(&outflow, &inflow).await;
    assert_eq!(link["one_movement"], false);

    assert_eq!(world.october().await, before);
}

/// The worked case: an auto-invest and a mortgage payment are spending, linked or not. Neither
/// account is in counted cash, though both are the household's.
#[tokio::test]
async fn a_link_to_an_invested_or_long_term_account_changes_no_number() {
    let world = world().await;
    let checking = world.checking().await;
    let brokerage = world.holding("Brokerage", "brokerage").await;
    let loan = world.holding("Home loan", "mortgage").await;
    let invest = world.expense("Investing").await;
    let house = world.expense("Mortgage").await;
    let auto = world.posted(checking, "2026-10-05", -500_000, invest).await;
    let bought = world.manual(brokerage, "2026-10-06", 500_000).await;
    let paid = world.posted(checking, "2026-10-07", -200_000, house).await;
    let owed = world.manual(loan, "2026-10-08", 200_000).await;
    let before = world.october().await;

    assert_eq!(world.linked(&auto, &bought).await["one_movement"], false);
    assert_eq!(world.linked(&paid, &owed).await["one_movement"], false);

    assert_eq!(world.october().await, before);
    assert_eq!(row(&before, OCTOBER, invest)["actual"], usd(500_000));
}

/// Closing an account ends what its links say, read at the time as every count is.
#[tokio::test]
async fn a_link_to_a_closed_account_changes_no_number() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let food = world.expense("Food").await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, food).await;
    let inflow = world.manual(savings, "2026-10-05", 50_000).await;
    world.linked(&outflow, &inflow).await;
    assert!(is_empty(&world.october().await, "rows"), "one movement");

    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{savings}"),
            &json!({"closed": true}),
        )
        .await;

    assert_eq!(
        row(&world.october().await, OCTOBER, food)["actual"],
        usd(50_000)
    );
}

/// Any link says where an uncategorized outflow went, so it leaves that list, and only what the
/// link says: 7.00 was sent and 3.00 arrived, and 4.00 stays asked about.
#[tokio::test]
async fn any_link_takes_what_it_says_out_of_uncategorized() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("College fund", "education").await;
    let outflow = world.manual(checking, "2026-10-06", -700).await;
    let inflow = world.manual(fund, "2026-10-06", 300).await;
    assert_eq!(world.october().await["uncategorized"][0]["total"], usd(700));

    world.linked(&outflow, &inflow).await;

    let report = world.october().await;
    assert_eq!(report["uncategorized"][0]["total"], usd(400));
    assert!(is_empty(&report, "moved_between_counted_accounts"));
}

/// Money between two accounts that aren't the household's stays in `not_the_households`.
#[tokio::test]
async fn a_link_between_two_held_accounts_changes_no_number() {
    let world = world().await;
    let fund = world.holding("College fund", "education").await;
    let gift = world.holding("Giving fund", "charitable").await;
    let college = world.expense("College").await;
    let outflow = world.posted(fund, "2026-10-05", -50_000, college).await;
    let inflow = world.posted(gift, "2026-10-05", 50_000, college).await;
    let before = world.october().await;

    world.linked(&outflow, &inflow).await;

    assert_eq!(world.october().await, before);
}

impl World {
    /// Says a fund is the household's own cash, which makes it counted.
    async fn counted_from_now_on(&self, fund: AccountId) {
        self.admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{fund}"),
            &json!({"belongs_to": "household", "liquidity": "cash"}),
        )
        .await;
    }
}

/// A link reads the accounts when the report is read, as every count does (spec §8.11 B6). A
/// contribution linked to a fund and to a goal is spending against the goal, until the fund is
/// said to be the household's cash: then it never left, and the goal's line says so.
#[tokio::test]
async fn a_link_follows_the_accounts() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("College fund", "education").await;
    let college = world.expense("College").await;
    let tuition = world.item(&goal("Tuition", 100_000, college)).await;
    let outflow = world
        .posted_linked(checking, "2026-10-05", -50_000, college, &tuition)
        .await;
    let inflow = world.manual(fund, "2026-10-05", 50_000).await;
    world.linked(&outflow, &inflow).await;

    let before = world.october().await;
    assert_eq!(before["goals"][0]["spent_against"], usd(50_000));
    assert!(is_empty(&before, "moved_between_counted_accounts"));

    world.counted_from_now_on(fund).await;

    let after = world.october().await;
    assert_eq!(after["goals"][0]["spent_against"], usd(0));
    assert_eq!(after["goals"][0]["left_out"], usd(50_000));
    assert_eq!(
        after["moved_between_counted_accounts"],
        moved(50_000, 50_000)
    );
}

/// A refund on a goal that turns out to have only moved was no refund, and isn't a charge left
/// out either: `left_out` holds charges, so it doesn't go negative.
#[tokio::test]
async fn a_refund_that_moved_leaves_nothing_left_out() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("College fund", "education").await;
    let college = world.expense("College").await;
    let tuition = world.item(&goal("Tuition", 100_000, college)).await;
    let refund = world
        .posted_linked(checking, "2026-10-05", 50_000, college, &tuition)
        .await;
    let paid_back = world.manual(fund, "2026-10-05", -50_000).await;
    world.linked(&paid_back, &refund).await;
    assert_eq!(
        world.october().await["goals"][0]["spent_against"],
        usd(-50_000)
    );

    world.counted_from_now_on(fund).await;

    let line = world.october().await;
    assert_eq!(line["goals"][0]["spent_against"], usd(0));
    assert_eq!(line["goals"][0]["left_out"], usd(0));
}
