//! Money that isn't the household's in the report (spec §8.11 B9, B10): what goes in is
//! spending, what happens inside is left out of `actual` and said once beside it.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::AccountId;
use serde_json::{Value, json};

use super::{PENDING, REMOVED, World, budget, goal, is_empty, monthly, row, usd, world};

const OCTOBER: &str = "2026-10";

/// What `not_the_households` says of October in USD, and nothing else.
fn says(net_minor: i64) -> Value {
    json!([{"month": OCTOBER, "net": usd(net_minor)}])
}

impl World {
    /// A 529 in USD, by its kind held for someone else.
    async fn college_fund(&self) -> AccountId {
        self.holding("College fund", "education").await
    }

    /// Patches a holding's `belongs_to`.
    async fn belonging_to(&self, holding: AccountId, belongs_to: &str) {
        self.admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{holding}"),
            &json!({"belongs_to": belongs_to}),
        )
        .await;
    }
}

/// A contribution leaves counted cash: it is spending, and it meets its bill.
#[tokio::test]
async fn a_contribution_reads_as_actual() {
    let world = world().await;
    let checking = world.checking().await;
    let college = world.expense("College").await;
    world
        .item(&monthly(
            "College fund contribution",
            "outflow",
            50_000,
            college,
            "2026-01-05",
        ))
        .await;
    world.posted(checking, "2026-10-05", -50_000, college).await;

    let report = world.october().await;

    let row = row(&report, OCTOBER, college);
    assert_eq!(row["actual"], usd(50_000));
    assert_eq!(row["difference"], usd(0));
    assert!(is_empty(&report, "not_the_households"), "its own money");
}

/// 500.00 goes in and 300.00 of it is granted, both in Giving: spent once, not 800.00.
#[tokio::test]
async fn a_grant_from_a_fund_is_not_spending_twice() {
    let world = world().await;
    let checking = world.checking().await;
    let fund = world.holding("Giving fund", "charitable").await;
    let giving = world.expense("Giving").await;
    world.posted(checking, "2026-10-05", -50_000, giving).await;
    world.posted(fund, "2026-10-20", -30_000, giving).await;

    let report = world.october().await;

    assert_eq!(row(&report, OCTOBER, giving)["actual"], usd(50_000));
    assert_eq!(report["not_the_households"], says(-30_000));
}

#[tokio::test]
async fn an_income_split_in_a_bucket_is_left_out() {
    let world = world().await;
    let fund = world.college_fund().await;
    let earned = world.category("Earned", "income").await;
    world.posted(fund, "2026-10-20", 20_000, earned).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "not the household's income");
    assert_eq!(report["not_the_households"], says(20_000));
}

/// A household's money it would rather not count is still what the household spent.
#[tokio::test]
async fn an_excluded_household_account_keeps_counting() {
    let world = world().await;
    let answer = world
        .admin_send(
            Method::POST,
            "/api/v1/accounts",
            &json!({
                "name": "Side",
                "kind": "checking",
                "currency": "USD",
                "include_in_net_worth": false,
                "include_in_forecast": false,
            }),
        )
        .await;
    let side: AccountId = serde_json::from_value(answer["id"].clone()).unwrap();
    let food = world.expense("Food").await;
    world.posted(side, "2026-10-06", -500, food).await;

    let report = world.october().await;

    assert_eq!(row(&report, OCTOBER, food)["actual"], usd(500));
    assert!(is_empty(&report, "not_the_households"), "its own money");
}

/// Of a spendable class, so only whose money it is keeps it out of the list.
#[tokio::test]
async fn uncategorized_money_in_a_held_savings_account_is_left_out() {
    let world = world().await;
    let answer = world
        .admin_send(
            Method::POST,
            "/api/v1/accounts",
            &json!({
                "name": "Held savings",
                "kind": "savings",
                "currency": "USD",
                "belongs_to": "someone_else",
            }),
        )
        .await;
    let held: AccountId = serde_json::from_value(answer["id"].clone()).unwrap();
    world.manual(held, "2026-10-06", -50_000).await;

    let report = world.october().await;

    assert!(is_empty(&report, "uncategorized"), "not the household's");
    assert!(is_empty(&report, "not_the_households"), "no category");
}

#[tokio::test]
async fn transfers_inside_a_bucket_say_nothing() {
    let world = world().await;
    let fund = world.college_fund().await;
    let moved = world.category("Moved", "transfer").await;
    world.posted(fund, "2026-10-06", -500, moved).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "moved money");
    assert!(is_empty(&report, "not_the_households"), "moved money");
}

/// A member linked it on purpose, and it is on the goal's line alone.
#[tokio::test]
async fn a_goal_split_in_a_bucket_stays_on_its_line() {
    let world = world().await;
    let fund = world.college_fund().await;
    let tuition = world.expense("Tuition").await;
    let first_year = world.item(&goal("First year", 1_000, tuition)).await;
    world
        .posted_linked(fund, "2026-10-06", -500, tuition, &first_year)
        .await;

    let report = world.october().await;

    assert_eq!(report["goals"][0]["spent_against"], usd(500));
    assert!(is_empty(&report, "rows"), "only on the goal's line");
    assert!(is_empty(&report, "not_the_households"), "only there");
}

/// The average reads September, and November isn't asked for.
#[tokio::test]
async fn months_outside_the_range_are_left_out() {
    let world = world().await;
    let fund = world.college_fund().await;
    let giving = world.expense("Giving").await;
    world.posted(fund, "2026-09-20", -100, giving).await;
    world.posted(fund, "2026-10-20", -30, giving).await;
    world.posted(fund, "2026-11-02", -7, giving).await;

    let report = world.report("from=2026-10&months=1&average_months=2").await;

    assert_eq!(report["not_the_households"], says(-30));
}

/// The plan has no history, and neither has whose money an account is.
#[tokio::test]
async fn moving_an_account_moves_its_past() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    world.posted(checking, "2026-10-06", -500, food).await;
    assert_eq!(
        row(&world.october().await, OCTOBER, food)["actual"],
        usd(500)
    );

    world.belonging_to(checking, "someone_else").await;

    let moved = world.october().await;
    assert!(is_empty(&moved, "rows"), "no longer the household's");
    assert_eq!(moved["not_the_households"], says(-500));

    world.belonging_to(checking, "household").await;

    let back = world.october().await;
    assert_eq!(row(&back, OCTOBER, food)["actual"], usd(500));
    assert!(is_empty(&back, "not_the_households"), "its own money again");
}

#[tokio::test]
async fn a_pending_split_in_a_bucket_says_nothing() {
    let world = world().await;
    let fund = world.college_fund().await;
    let giving = world.expense("Giving").await;
    let pending = world.posted(fund, "2026-10-20", -30, giving).await;
    world.plant(PENDING, &pending).await;

    assert!(
        is_empty(&world.october().await, "not_the_households"),
        "not posted yet"
    );
}

/// It says what is left out, not what it meant, so a flag changes nothing here.
#[tokio::test]
async fn a_flagged_split_in_a_bucket_is_still_said() {
    let world = world().await;
    let fund = world.college_fund().await;
    let giving = world.expense("Giving").await;
    let removed = world.posted(fund, "2026-10-20", -30, giving).await;
    world.plant(REMOVED, &removed).await;

    assert_eq!(world.october().await["not_the_households"], says(-30));
}

/// A budget's rollover is what the household spent on it.
#[tokio::test]
async fn a_split_in_a_bucket_spends_no_budget() {
    let world = world().await;
    let fund = world.college_fund().await;
    let food = world.expense("Food").await;
    world.item(&budget("Groceries", 1_000, food)).await;
    world.posted(fund, "2026-10-20", -400, food).await;

    let report = world.october().await;

    assert_eq!(report["rollovers"][0]["spent"], usd(0));
    assert_eq!(report["rollovers"][0]["rollover"], usd(1_000));
}

#[tokio::test]
async fn the_list_goes_by_month_and_currency() {
    let world = world().await;
    let dollars = world.college_fund().await;
    let euros = world.holding_in("In euros", "custodial", "EUR").await;
    let giving = world.expense("Giving").await;
    world.posted(dollars, "2026-11-02", -1, giving).await;
    world.posted(dollars, "2026-10-02", -20, giving).await;
    world.posted(euros, "2026-10-03", -300, giving).await;

    let report = world.report("from=2026-10&months=2&average_months=1").await;

    assert_eq!(
        report["not_the_households"],
        json!([
            {"month": OCTOBER, "net": {"minor_units": -300, "currency": "EUR"}},
            {"month": OCTOBER, "net": usd(-20)},
            {"month": "2026-11", "net": usd(-1)},
        ])
    );
}

#[tokio::test]
async fn a_total_past_the_range_is_refused() {
    let world = world().await;
    let fund = world.college_fund().await;
    let giving = world.expense("Giving").await;
    for _ in 0..2 {
        world
            .posted(fund, "2026-10-20", -9_000_000_000_000_000_000, giving)
            .await;
    }

    let (status, text) = world
        .send(
            Method::GET,
            "/api/v1/reports/plan-vs-actual?from=2026-10&months=1&average_months=1",
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body["detail"],
        "the report's totals are too large to represent"
    );
}
