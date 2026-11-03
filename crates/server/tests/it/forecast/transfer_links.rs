//! Linked transfers in the forecast (spec §8.12 T6): one movement between the household's
//! accounts lowers no budget and pays no bill, and a link out of it changes nothing.

use axum::http::Method;
use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::{AS_OF, BALANCE_ON, World, caution, kinds, line, monthly, outflow, usd, with, world};

const ONE_MONTH: &str = "?horizon_months=1";

fn totals(forecast: &Value) -> &Value {
    let [only] = forecast["totals"].as_array().unwrap().as_slice() else {
        panic!("expected one currency");
    };
    only
}

impl World {
    /// `minor_units` into `account`, as an uncategorized transaction; returns its id.
    async fn deposit(&self, account: AccountId, posted_on: &str, minor_units: i64) -> String {
        self.manual(account, posted_on, minor_units)
            .await
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// `minor_units` out of `account`, one split in `category`; returns its id.
    async fn charge(
        &self,
        account: AccountId,
        posted_on: &str,
        minor_units: i64,
        category: CategoryId,
    ) -> String {
        self.spend_linked(account, posted_on, minor_units, category, &Value::Null)
            .await
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn link_legs(&self, outflow: &str, inflow: &str) -> Value {
        self.admin_send(
            Method::POST,
            "/api/v1/transfer-links",
            &json!({"outflow": outflow, "inflow": inflow}),
        )
        .await
    }

    /// Checking at 2,000.00 and Savings at 1,000.00, both as of the worked example's balances,
    /// with no floor to hold.
    async fn two_accounts(&self) -> (AccountId, AccountId) {
        self.floor(0).await;
        (
            self.funded("Checking", 200_000).await,
            self.funded("Savings", 100_000).await,
        )
    }
}

/// A budget of 400.00 with 300.00 filed against it, all shown by the balances. Unlinked, that
/// is spending, so 100.00 is left to hold. Linked to a deposit into Savings, it is money still
/// held: cash is the same 3,000.00, the whole 400.00 is needed again, and the surplus falls by
/// the 300.00 it had overstated.
#[tokio::test]
async fn a_linked_leg_lowers_no_budget() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let out = world.charge(checking, "2026-11-05", 30_000, food).await;
    let into = world.deposit(savings, "2026-11-05", 30_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["cash_available"], usd(300_000));
    assert_eq!(line(&before, "Groceries")["reduced_by"], usd(30_000));

    world.link_legs(&out, &into).await;

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&after)["cash_available"], usd(300_000));
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(0));
    let surplus = |forecast: &Value| forecast["surplus"]["minor_units"].as_i64().unwrap();
    assert_eq!(surplus(&after), surplus(&before) - 30_000);
}

/// A charge in a bill's category pays it, until a person says the money only moved: then the
/// bill is unpaid again, with the caution saying so.
#[tokio::test]
async fn a_linked_leg_pays_no_bill() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let rent = world.expense("Rent").await;
    let mut bill = outflow("Rent", 50_000, rent, &monthly("2026-11-05"));
    bill["schedule"]["end_on"] = json!("2026-11-30");
    world.item(&bill).await;
    let out = world.charge(checking, "2026-11-05", 50_000, rent).await;
    let into = world.deposit(savings, "2026-11-05", 50_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["planned_outflows"], usd(0));

    world.link_legs(&out, &into).await;

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&after, "Rent")["unpaid"], usd(50_000));
    assert_eq!(caution(&after, "bill_not_seen")["amount"], usd(50_000));
}

/// `uncategorized_spending` asks a person to say what money was. A link has said.
#[tokio::test]
async fn a_linked_leg_raises_no_uncategorized_spending() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let out = world.deposit(checking, "2026-11-05", -50_000).await;
    let into = world.deposit(savings, "2026-11-05", 50_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(kinds(&before, "cautions"), ["uncategorized_spending"]);

    world.link_legs(&out, &into).await;

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(after["cautions"], json!([]));
    assert_eq!(after["status"], "firm");
}

/// Spec §8.11 B9: into a fund, it is spending, so a bill is paid and the number stays what it
/// was before the link, cautions and all.
#[tokio::test]
async fn a_link_out_of_the_household_changes_no_number() {
    let world = world().await;
    world.floor(0).await;
    let checking = world.funded("Checking", 200_000).await;
    let fund = world
        .account_with("College fund", "education", "USD", json!({}))
        .await;
    let college = world.expense("College").await;
    let mut bill = outflow("Contribution", 50_000, college, &monthly("2026-11-05"));
    bill["schedule"]["end_on"] = json!("2026-11-30");
    world.item(&bill).await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let out = world.charge(checking, "2026-11-05", 50_000, college).await;
    let into = world.deposit(fund, "2026-11-05", 50_000).await;
    let plain_out = world.deposit(checking, "2026-11-06", -700).await;
    let plain_in = world.deposit(fund, "2026-11-06", 700).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        kinds(&before, "cautions"),
        ["uncategorized_spending"],
        "the bill is paid, and the plain outflow isn't filed"
    );

    world.link_legs(&out, &into).await;
    world.link_legs(&plain_out, &plain_in).await;

    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// A contribution linked to a goal lowers its need, until the fund is said to be the
/// household's: then nothing left, and the need comes back. Read when asked, as every count.
#[tokio::test]
async fn a_goal_counts_a_leg_only_while_the_money_leaves() {
    let world = world().await;
    world.floor(0).await;
    let checking = world.funded("Checking", 300_000).await;
    let fund = world
        .account_with("College fund", "education", "USD", json!({}))
        .await;
    let college = world.expense("College").await;
    let goal = world
        .item(&outflow(
            "Tuition",
            120_000,
            college,
            &json!({"type": "goal", "start_on": "2026-01-01", "due_on": "2027-12-31"}),
        ))
        .await;
    let out = world
        .spend_linked(checking, "2026-11-05", 50_000, college, &goal)
        .await;
    let into = world.deposit(fund, "2026-11-05", 50_000).await;
    world.link_legs(out.as_str().unwrap(), &into).await;

    let minor =
        |forecast: &Value, field: &str| totals(forecast)[field]["minor_units"].as_i64().unwrap();
    let leaving = world.forecast_at(ONE_MONTH).await;

    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{fund}"),
            &json!({"belongs_to": "household"}),
        )
        .await;

    let inside = world.forecast_at(ONE_MONTH).await;
    assert_eq!(minor(&inside, "goals"), minor(&leaving, "goals") + 50_000);
}

/// A balance that hasn't shown the money may still be the one that matters, and a link doesn't
/// say which sides have moved, so the caution stands (R10).
#[tokio::test]
async fn a_link_doesnt_quiet_a_balance_behind_its_money() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    assert!(BALANCE_ON < AS_OF);
    let moved = world.category("Moved", "transfer").await;
    let out = world.charge(checking, AS_OF, 30_000, moved).await;
    let into = world.deposit(savings, AS_OF, 30_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&before, "spending_newer_than_balance")["total"],
        usd(30_000)
    );

    world.link_legs(&out, &into).await;

    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// A closed account's spending was never in the budget (R10), but a leg on an open account
/// linked to one is still a movement: the money went somewhere of the household's.
#[tokio::test]
async fn a_leg_on_a_closed_account_still_makes_a_movement() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let out = world.charge(checking, "2026-11-05", 30_000, food).await;
    let into = world.deposit(savings, "2026-11-05", 30_000).await;
    world.close(savings).await;

    world.link_legs(&out, &into).await;

    assert_eq!(
        line(&world.forecast_at(ONE_MONTH).await, "Groceries")["reduced_by"],
        usd(0)
    );
}
