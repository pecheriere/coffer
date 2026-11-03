//! Linked transfers in the report (spec §8.12 T5): one movement inside the household is neither
//! spending nor income, and says so beside the rows. A link out of it changes nothing.

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

/// What `moved_within_household` says of October in USD, and nothing else.
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
    assert!(is_empty(&before, "moved_within_household"));

    let link = world.linked(&outflow, &inflow).await;

    let after = world.october().await;
    assert_eq!(row(&after, OCTOBER, food)["actual"], usd(3_000));
    let rows = after["rows"].as_array().unwrap();
    assert!(
        rows.iter().all(|row| row["category"] != json!(pay)),
        "pay has nothing left to say"
    );
    assert_eq!(after["moved_within_household"], moved(50_000, 50_000));

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
    assert_eq!(after["moved_within_household"], moved(50_000, 50_000));
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
    assert_eq!(after["moved_within_household"], moved(50_000, 50_000));
}

/// A fee is what the link's own `difference` states, and the report's two sides show it too,
/// rather than the money going quietly missing.
#[tokio::test]
async fn a_fee_shows_between_the_two_sides() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.savings().await;
    let outflow = world.manual(checking, "2026-10-05", -50_000).await;
    let inflow = world.manual(savings, "2026-10-05", 49_900).await;

    world.linked(&outflow, &inflow).await;

    assert_eq!(
        world.october().await["moved_within_household"],
        moved(50_000, 49_900)
    );
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
        report["moved_within_household"],
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

    assert!(is_empty(&report, "moved_within_household"));
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
        world.october().await["moved_within_household"],
        moved(0, 50_000)
    );
}

/// Spec §8.11 B9: money leaving the household is spending. The link records the movement and
/// every number stays what it was, on both sides of the boundary.
#[tokio::test]
async fn a_link_out_of_the_household_changes_no_number() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("College fund", "education").await;
    let college = world.expense("College").await;
    let outflow = world.posted(checking, "2026-10-05", -50_000, college).await;
    let inflow = world.posted(fund, "2026-10-05", 50_000, college).await;
    let plain = world.manual(checking, "2026-10-06", -700).await;
    let plain_in = world.manual(fund, "2026-10-06", 700).await;
    let before = world.october().await;
    assert_eq!(row(&before, OCTOBER, college)["actual"], usd(50_000));
    assert_eq!(before["not_the_households"][0]["net"], usd(50_000));

    let link = world.linked(&outflow, &inflow).await;
    assert_eq!(link["within_household"], false);
    world.linked(&plain, &plain_in).await;

    assert_eq!(world.october().await, before);
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

/// A link reads whose money each account is when the report is read, as every count does (spec
/// §8.11 B6). A contribution linked to a fund and to a goal is spending against the goal, until
/// the fund is said to be the household's: then it never left, and the goal's line says so.
#[tokio::test]
async fn a_link_follows_whose_money_it_is() {
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
    assert!(is_empty(&before, "moved_within_household"));

    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{fund}"),
            &json!({"belongs_to": "household"}),
        )
        .await;

    let after = world.october().await;
    assert_eq!(after["goals"][0]["spent_against"], usd(0));
    assert_eq!(after["goals"][0]["left_out"], usd(50_000));
    assert_eq!(after["moved_within_household"], moved(50_000, 50_000));
}
