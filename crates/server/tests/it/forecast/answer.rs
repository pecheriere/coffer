//! The number itself, end to end: what reaches it from the API and what doesn't.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::transactions::SplitInput;
use coffer_server::testkit::TestServer;
use coffer_store::{Attribution, NewTransaction};
use serde_json::{Value, json};

use super::{
    AS_OF, BALANCE_ON, FORECAST, World, caution, goal_schedule, kinds, line, monthly, noon,
    one_off_on, outflow, usd, with, world, world_on,
};

/// The household of the worked example (the domain's `golden.rs`), through the API.
pub(crate) struct Golden {
    pub(crate) world: World,
    pub(crate) checking: AccountId,
    side: AccountId,
    pub(crate) utilities: CategoryId,
    vacation: Value,
}

pub(crate) async fn golden() -> Golden {
    let world = world().await;
    let checking = world.funded("Checking", 2_000_000).await;
    world.funded("Savings", 5_000_000).await;
    let card = world
        .account_with("Card", "credit_card", "USD", json!({}))
        .await;
    world.balance(card, BALANCE_ON, -150_000).await;
    let brokerage = world
        .account_with("Brokerage", "brokerage", "USD", json!({}))
        .await;
    world.balance(brokerage, BALANCE_ON, 9_000_000).await;
    let side = world
        .account_with(
            "Side",
            "checking",
            "USD",
            json!({"include_in_forecast": false}),
        )
        .await;
    world.balance(side, BALANCE_ON, 100_000).await;

    let housing = world.expense("Housing").await;
    let food = world.expense("Food").await;
    let utilities = world.expense("Utilities").await;
    let car = world.expense("Car").await;
    let travel = world.expense("Travel").await;
    let home = world.expense("Home").await;
    let pay = world.category("Pay", "income").await;

    world
        .item(&outflow("Rent", 200_000, housing, &monthly("2026-01-01")))
        .await;
    world
        .item(&with(
            outflow("Groceries", 60_000, food, &monthly("2026-11-28")),
            "spending",
            json!("budget"),
        ))
        .await;
    world
        .item(&outflow(
            "Water",
            12_000,
            utilities,
            &json!({"type": "recurring", "every": 2, "unit": "months", "anchor_on": "2026-01-10"}),
        ))
        .await;
    world
        .item(&outflow("Repair", 150_000, car, &one_off_on("2026-12-20")))
        .await;
    world
        .item(&outflow("Late", 99_900, car, &one_off_on("2027-02-15")))
        .await;
    let vacation = world
        .item(&outflow(
            "Vacation",
            600_000,
            travel,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    world
        .item(&outflow(
            "Roof",
            1_200_000,
            home,
            &goal_schedule("2026-02-15", "2028-02-15"),
        ))
        .await;
    world
        .item(&json!({
            "name": "Salary",
            "category": pay,
            "amount": usd(800_000),
            "direction": "inflow",
            "schedule": monthly("2026-11-25"),
        }))
        .await;

    world.emergency_fund(1_000_000).await;
    // All of it on or before the balance date, so the balances already show it.
    world.spend(checking, "2026-10-01", 200_000, housing).await;
    world.spend(checking, "2026-11-01", 200_000, housing).await;
    world.spend(checking, "2026-11-05", 40_000, food).await;
    world
        .spend_linked(checking, "2026-11-06", 100_000, travel, &vacation)
        .await;
    Golden {
        world,
        checking,
        side,
        utilities,
        vacation,
    }
}

/// The hand check, from `as_of` 2026-11-15 over three months, so before 2027-02-15:
///
/// - Cash: Checking 2,000,000 + Savings 5,000,000 + Card -150,000 = 6,850,000.
///   Brokerage isn't cash, and Side is switched out.
/// - Rent, a bill on the 1st: Dec, Jan, Feb = 600,000. Nov 1's 200,000 has passed, and
///   the 200,000 spent on Housing pays it.
/// - Groceries, a budget on the 28th: Nov, Dec, Jan = 180,000, less the 40,000 spent
///   on Food = 140,000.
/// - Water, a bill every 2 months from January: Jan 10 = 12,000 in the window, and Nov
///   10 has passed with nothing spent on Utilities, so 12,000 more is still needed.
/// - Repair: 150,000. Late falls on the window's end, which is left out.
/// - Outflows: 600,000 + 140,000 + 24,000 + 150,000 = 914,000.
/// - Vacation is due in the window: 600,000 - 100,000 linked to it = 500,000.
/// - Roof, 1,200,000 over 730 days, is 365 days along at the window's end: 600,000.
/// - Goals: 1,100,000. Emergency fund: 1,000,000.
/// - Needed: 914,000 + 1,100,000 + 1,000,000 = 3,014,000.
/// - Surplus: 6,850,000 - 3,014,000 = 3,836,000, provisional for the water bill.
/// - With income, Salary on the 25th three times: needed 3,014,000 - 2,400,000 =
///   614,000, surplus 6,236,000.
/// - With 12,000 spent on Utilities: no gap, outflows 902,000, needed 3,002,000, surplus
///   3,848,000, firm.
#[tokio::test]
async fn forecast_over_the_api_matches_the_golden_example() {
    let Golden {
        world,
        checking,
        utilities,
        ..
    } = golden().await;

    let briefed = world.forecast().await;
    assert_eq!(briefed["as_of"], AS_OF, "the household's today");
    assert_eq!(briefed["end_exclusive"], "2027-02-15", "three months on");
    assert_eq!(briefed["status"], "provisional");
    assert_eq!(briefed["surplus"], usd(3_836_000));
    let totals = &briefed["totals"][0];
    assert_eq!(totals["cash_available"], usd(6_850_000));
    assert_eq!(totals["planned_outflows"], usd(914_000));
    assert_eq!(totals["goals"], usd(1_100_000));
    assert_eq!(totals["emergency_fund"], usd(1_000_000));
    assert_eq!(totals["cash_needed"], usd(3_014_000));
    assert_eq!(kinds(&briefed, "cautions"), ["bill_not_seen"]);
    let not_seen = caution(&briefed, "bill_not_seen");
    assert_eq!(not_seen["category"], json!(utilities));
    assert_eq!(not_seen["amount"], usd(12_000));

    let with_income = world.forecast_at("?include_income=true").await;
    assert_eq!(with_income["totals"][0]["cash_needed"], usd(614_000));
    assert_eq!(with_income["surplus"], usd(6_236_000));
    assert_eq!(with_income["status"], "provisional");

    world.spend(checking, "2026-11-10", 12_000, utilities).await;
    let seen = world.forecast().await;
    assert_eq!(seen["totals"][0]["cash_needed"], usd(3_002_000));
    assert_eq!(seen["surplus"], usd(3_848_000));
    assert_eq!(seen["status"], "firm");
    assert_eq!(seen["cautions"], json!([]));
}

/// A balance dated the 14th doesn't show a payment posted on the 15th, so counting it
/// as paying the bill would count it twice in the household's favour.
#[tokio::test]
async fn spending_newer_than_the_balance_leaves_the_number_alone() {
    let Golden {
        world,
        checking,
        utilities,
        ..
    } = golden().await;
    world.spend(checking, AS_OF, 12_000, utilities).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(3_836_000));
    assert_eq!(caution(&forecast, "bill_not_seen")["amount"], usd(12_000));
}

#[tokio::test]
async fn goal_spending_newer_than_the_balance_leaves_the_need_alone() {
    let Golden {
        world,
        checking,
        vacation,
        ..
    } = golden().await;
    let travel: CategoryId =
        serde_json::from_value(line(&world.forecast().await, "Vacation")["category"].clone())
            .unwrap();
    world
        .spend_linked(checking, AS_OF, 50_000, travel, &vacation)
        .await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Vacation")["counted"], usd(500_000));
    assert_eq!(forecast["surplus"], usd(3_836_000));
}

/// It never touched counted cash, so the balance date of no counted account limits it,
/// and its own balance date doesn't either.
#[tokio::test]
async fn goal_spending_from_an_excluded_account_lowers_the_need_at_once() {
    let Golden {
        world,
        side,
        vacation,
        ..
    } = golden().await;
    let travel: CategoryId =
        serde_json::from_value(line(&world.forecast().await, "Vacation")["category"].clone())
            .unwrap();
    world
        .spend_linked(side, AS_OF, 50_000, travel, &vacation)
        .await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Vacation")["counted"], usd(450_000));
    assert_eq!(forecast["surplus"], usd(3_886_000));
}

#[tokio::test]
async fn today_uses_the_household_time_zone() {
    for (zone, today) in [
        ("Pacific/Auckland", "2026-03-01"),
        ("America/Los_Angeles", "2026-02-28"),
    ] {
        let world = world_on(TestServer::with_env(&[("COFFER_TIMEZONE", zone)]).await).await;
        world
            .server
            .clock
            .set("2026-03-01T02:00:00Z".parse().unwrap());
        let forecast = world.forecast().await;
        assert_eq!(forecast["as_of"], today, "the household's day");
        assert!(
            !kinds(&forecast, "cautions").contains(&"as_of_not_today".to_owned()),
            "the household's day is today"
        );
    }
}

#[tokio::test]
async fn empty_household_has_no_number() {
    let world = world().await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "unavailable");
    assert_eq!(forecast["surplus"], Value::Null);
    assert_eq!(
        kinds(&forecast, "blockers"),
        ["no_counted_accounts"],
        "no currency is in use, so none lacks an emergency fund"
    );
}

#[tokio::test]
async fn the_forecast_no_longer_blocks_without_a_target() {
    let world = world().await;
    world.funded("Checking", 1_000).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["status"], "provisional");
    assert_eq!(
        forecast["surplus"],
        json!({"minor_units": 1_000, "currency": "USD"}),
        "nothing held back"
    );
    assert_eq!(forecast["blockers"], json!([]));
    assert_eq!(
        forecast["cautions"],
        json!([{"kind": "emergency_fund_not_set", "currency": "USD"}])
    );
}

#[tokio::test]
async fn settings_supply_the_defaults() {
    let world = world().await;
    world
        .settings(&json!({
            "include_income": true,
            "default_horizon_months": 6,
            "stale_after_days": 30,
        }))
        .await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["horizon_months"], 6);
    assert_eq!(forecast["end_exclusive"], "2027-05-15");
    assert_eq!(forecast["include_income"], true);
    assert_eq!(forecast["stale_after_days"], 30);
}

#[tokio::test]
async fn query_overrides_settings() {
    let world = world().await;
    world
        .settings(&json!({"include_income": true, "default_horizon_months": 6}))
        .await;

    let forecast = world
        .forecast_at("?as_of=2026-12-01&horizon_months=2&include_income=false")
        .await;

    assert_eq!(forecast["as_of"], "2026-12-01");
    assert_eq!(forecast["end_exclusive"], "2027-02-01");
    assert_eq!(forecast["horizon_months"], 2);
    assert_eq!(forecast["include_income"], false);
    assert_eq!(
        caution(&forecast, "as_of_not_today")["today"],
        AS_OF,
        "a window that doesn't start today says so"
    );
}

#[tokio::test]
async fn horizon_out_of_range_is_422() {
    let world = world().await;
    for horizon in [0, 61] {
        let uri = format!("{FORECAST}?horizon_months={horizon}");
        let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["detail"], "the horizon must be 1 to 60 months");
    }
}

#[tokio::test]
async fn as_of_out_of_range_is_422() {
    let world = world().await;
    for as_of in ["1969-12-31", "2201-01-01"] {
        let uri = format!("{FORECAST}?as_of={as_of}");
        let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!text.contains(as_of), "the refusal echoes nothing");
    }
}

#[tokio::test]
async fn as_of_must_be_a_plain_date() {
    let world = world().await;
    for as_of in ["2026-11-15T00:00:00Z", "2026-11-15T00:00:00", "20261115"] {
        let uri = format!("{FORECAST}?as_of={}", as_of.replace(':', "%3A"));
        let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "refused in a query");
        assert!(!text.contains(as_of), "the refusal echoes nothing");
        let (status, text) = world
            .send(
                Method::POST,
                super::WHAT_IF,
                &world.admin,
                Some(&json!({"as_of": as_of})),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "refused in a body");
        assert!(!text.contains(as_of), "the refusal echoes nothing");
    }
}

/// Six items of 10^15 every day for 60 months are 6 × 1826 × 10^15, past `i64`.
#[tokio::test]
async fn totals_too_large_are_422() {
    let world = world().await;
    let category = world.expense("Everything").await;
    world.funded("Checking", 1).await;
    world.emergency_fund(0).await;
    let daily = json!({"type": "recurring", "every": 1, "unit": "days", "anchor_on": AS_OF});
    for _ in 0..6 {
        world
            .item(&outflow("Huge", 1_000_000_000_000_000, category, &daily))
            .await;
    }

    let uri = format!("{FORECAST}?horizon_months=60");
    let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        body["detail"],
        "the forecast's totals are too large to represent"
    );
}

/// A budget of 60,000 and 4,000 that counts, among spending that doesn't: pending,
/// from an excluded account, from a closed one, and linked to a goal.
#[tokio::test]
async fn actuals_ignore_pending_excluded_accounts_and_goal_links() {
    let world = world().await;
    let food = world.expense("Food").await;
    let checking = world.funded("Checking", 1_000_000).await;
    let excluded = world
        .account_with(
            "Side",
            "checking",
            "USD",
            json!({"include_in_forecast": false}),
        )
        .await;
    world.balance(excluded, BALANCE_ON, 100).await;
    let closed = world.funded("Old", 100).await;
    let goal = world
        .item(&outflow(
            "Party",
            90_000,
            food,
            &goal_schedule("2026-01-01", "2027-01-01"),
        ))
        .await;
    world
        .item(&with(
            outflow("Groceries", 60_000, food, &monthly("2026-11-28")),
            "spending",
            json!("budget"),
        ))
        .await;
    world.emergency_fund(0).await;

    world.spend(checking, "2026-11-02", 4_000, food).await;
    world.spend(excluded, "2026-11-03", 2_000, food).await;
    world.spend(closed, "2026-11-04", 8_000, food).await;
    world.close(closed).await;
    world
        .spend_linked(checking, "2026-11-05", 3_000, food, &goal)
        .await;
    insert_pending(&world, checking, food, 1_000).await;

    let forecast = world.forecast().await;

    assert_eq!(line(&forecast, "Groceries")["reduced_by"], usd(4_000));
}

/// A pending charge, which only a provider can report: the API creates none.
pub(crate) async fn insert_pending(
    world: &World,
    account: AccountId,
    category: CategoryId,
    minor: i64,
) {
    let now = noon(AS_OF);
    let mut tx = world.server.store.begin().await.unwrap();
    let record = tx
        .insert_transaction(NewTransaction {
            origin: coffer_domain::transactions::TransactionOrigin::Imported,
            account,
            posted_on: "2026-11-06".parse().unwrap(),
            transacted_on: None,
            pending: true,
            amount_minor: -minor,
            currency: coffer_domain::money::Currency::parse("USD").unwrap(),
            raw_description: coffer_domain::transactions::Description::parse("Pending").unwrap(),
            imported_payee: None,
            memo: None,
            notes: None,
            created_at: now,
        })
        .await
        .unwrap();
    tx.replace_splits(
        record.id,
        &[SplitInput {
            category,
            minor_units: -minor,
            goal: None,
        }],
        Attribution {
            member: Some(world.alice.member),
            agent: None,
            system: None,
            rule: None,
            at: now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn goal_spending_reduces_remaining() {
    let world = world().await;
    let travel = world.expense("Travel").await;
    let checking = world.funded("Checking", 1_000_000).await;
    let goal = world
        .item(&outflow(
            "Vacation",
            600_000,
            travel,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    world
        .spend_linked(checking, "2026-10-01", 100_000, travel, &goal)
        .await;

    let forecast = world.forecast().await;

    let line = line(&forecast, "Vacation");
    assert_eq!(line["goal"]["spent"], usd(100_000));
    assert_eq!(line["counted"], usd(500_000));
}

/// Each on the 28th, each with 10,000 spent in its category this month.
#[tokio::test]
async fn a_stored_budget_is_reduced_and_a_stored_bill_is_not() {
    let world = world().await;
    let checking = world.funded("Checking", 1_000_000).await;
    let (bills, food) = (world.expense("Bills").await, world.expense("Food").await);
    let schedule = monthly("2026-11-28");
    world
        .item(&outflow("Power", 60_000, bills, &schedule))
        .await;
    world
        .item(&with(
            outflow("Groceries", 60_000, food, &schedule),
            "spending",
            json!("budget"),
        ))
        .await;
    world.spend(checking, "2026-11-02", 10_000, bills).await;
    world.spend(checking, "2026-11-02", 10_000, food).await;

    let forecast = world.forecast_at("?horizon_months=1").await;

    let (bill, budget) = (line(&forecast, "Power"), line(&forecast, "Groceries"));
    assert_eq!(bill["reduced_by"], usd(0));
    assert_eq!(bill["counted"], usd(60_000));
    assert_eq!(budget["reduced_by"], usd(10_000));
    assert_eq!(budget["counted"], usd(50_000));
}

#[tokio::test]
async fn future_dated_snapshot_is_ignored() {
    let world = world().await;
    let checking = world.funded("Checking", 1_000).await;
    world.balance(checking, "2026-12-01", 999_999).await;
    world.emergency_fund(0).await;

    let forecast = world.forecast().await;

    let counted = &forecast["accounts"][0];
    assert_eq!(counted["balance"], usd(1_000));
    assert_eq!(counted["balance_on"], BALANCE_ON);
    assert_eq!(forecast["surplus"], usd(1_000));
}
