//! Linked transfers in the forecast (spec §8.12 T6): one movement between counted accounts lowers
//! no budget and pays no bill, and any other link changes nothing.

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

/// The worked case. Checking held 10,000.00, and a 2,000.00 mortgage bill and a 5,000.00
/// auto-invest bill were both paid from it this month, so the balance is 3,000.00, nothing more
/// is owed and 3,000.00 is surplus. Linking each to where it went says only that, into an
/// invested and a long-term account, which aren't counted cash: treating them as movements
/// would owe both bills again, a surplus of -4,000.00, and might prompt a second contribution.
#[tokio::test]
async fn a_paid_bill_stays_paid_when_linked_out_of_counted_cash() {
    let world = world().await;
    world.floor(0).await;
    let checking = world.funded("Checking", 300_000).await;
    let brokerage = world
        .account_with("Brokerage", "brokerage", "USD", json!({}))
        .await;
    let loan = world
        .account_with("Home loan", "mortgage", "USD", json!({}))
        .await;
    let house = world.expense("Mortgage").await;
    let invest = world.expense("Investing").await;
    for (name, amount, category) in [("House", 200_000, house), ("Auto-invest", 500_000, invest)] {
        let mut bill = outflow(name, amount, category, &monthly("2026-11-05"));
        bill["schedule"]["end_on"] = json!("2026-11-30");
        world.item(&bill).await;
    }
    let house_out = world.charge(checking, "2026-11-05", 200_000, house).await;
    let invest_out = world.charge(checking, "2026-11-05", 500_000, invest).await;
    let house_in = world.deposit(loan, "2026-11-06", 200_000).await;
    let invest_in = world.deposit(brokerage, "2026-11-06", 500_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["cash_available"], usd(300_000));
    assert_eq!(totals(&before)["planned_outflows"], usd(0));
    assert_eq!(before["surplus"], usd(300_000));
    assert_eq!(before["cautions"], json!([]));

    assert_eq!(
        world.link_legs(&house_out, &house_in).await["one_movement"],
        false
    );
    assert_eq!(
        world.link_legs(&invest_out, &invest_in).await["one_movement"],
        false
    );

    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
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
    let out = world.charge(checking, "2026-11-05", 50_000, college).await;
    let into = world.deposit(fund, "2026-11-05", 50_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(totals(&before)["planned_outflows"], usd(0));

    world.link_legs(&out, &into).await;

    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// A contribution to a 529 or a brokerage is real spending (B9), so uncategorized it keeps the
/// caution until it is filed, whatever is linked, and a `transfer` category clears it.
#[tokio::test]
async fn a_link_out_of_counted_cash_keeps_uncategorized_spending_until_filed() {
    for kind in ["education", "brokerage"] {
        let world = world().await;
        world.floor(0).await;
        let checking = world.funded("Checking", 200_000).await;
        let other = world
            .account_with("Elsewhere", kind, "USD", json!({}))
            .await;
        let food = world.expense("Food").await;
        world
            .item(&with(
                outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
                "spending",
                json!("budget"),
            ))
            .await;
        let out = world.deposit(checking, "2026-11-05", -50_000).await;
        let into = world.deposit(other, "2026-11-06", 50_000).await;

        world.link_legs(&out, &into).await;

        let forecast = world.forecast_at(ONE_MONTH).await;
        assert_eq!(
            caution(&forecast, "uncategorized_spending")["total"],
            usd(50_000)
        );
        assert_eq!(forecast["status"], "provisional");

        let moved = world.category("Moved", "transfer").await;
        world
            .admin_send(
                Method::PATCH,
                &format!("/api/v1/transactions/{out}"),
                &json!({"splits": [{"category": moved, "minor_units": -50_000}]}),
            )
            .await;
        assert_eq!(world.forecast_at(ONE_MONTH).await["cautions"], json!([]));
    }
}

/// A difference in one currency stays spending: 1,250.00 linked to 12.50 lowers the budget by
/// 1,237.50, and nothing says the number is firm for the wrong reason.
#[tokio::test]
async fn only_what_moved_leaves_a_budget() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let food = world.expense("Food").await;
    world
        .item(&with(
            outflow("Groceries", 200_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let out = world.charge(checking, "2026-11-05", 125_000, food).await;
    let into = world.deposit(savings, "2026-11-05", 1_250).await;

    world.link_legs(&out, &into).await;

    assert_eq!(
        line(&world.forecast_at(ONE_MONTH).await, "Groceries")["reduced_by"],
        usd(123_750)
    );
}

/// A provider taking back a leg brings the money back and says so: the charge lowers the budget
/// again, and the forecast is provisional until a person looks. A link to an account that isn't
/// counted has nothing to say.
#[tokio::test]
async fn a_leg_taken_back_brings_the_money_back_with_a_caution() {
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
    let link = world.link_legs(&out, &into).await;
    let held = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&held, "Groceries")["reduced_by"], usd(0));
    assert_eq!(held["status"], "firm");

    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE transactions SET flag_source_removed = 1 WHERE id = ?",
            &into,
        )
        .await
        .unwrap();

    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(30_000));
    assert_eq!(after["status"], "provisional");
    assert_eq!(caution(&after, "transfer_leg_removed")["link"], link["id"]);
}

/// A pending charge its provider withdrew is no money at all, so the balance isn't missing it.
#[tokio::test]
async fn a_pending_leg_taken_back_isnt_newer_spending() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let out = world.deposit(checking, AS_OF, -30_000).await;
    let into = world.deposit(savings, BALANCE_ON, 30_000).await;
    world.link_legs(&out, &into).await;
    for statement in [
        "UPDATE transactions SET pending = 1 WHERE id = ?",
        "UPDATE transactions SET flag_source_removed = 1 WHERE id = ?",
    ] {
        world
            .server
            .store
            .execute_unchecked_for_tests(statement, &out)
            .await
            .unwrap();
    }

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        kinds(&forecast, "cautions"),
        ["transfer_leg_removed"],
        "the link still wants a look"
    );
}

/// A contribution linked to a goal lowers its need, until the fund is said to be the household's
/// cash: then nothing left, and the need comes back. Read when asked, as every count.
#[tokio::test]
async fn a_goal_counts_a_leg_only_while_the_money_leaves() {
    let world = world().await;
    world.floor(0).await;
    let checking = world.funded("Checking", 300_000).await;
    let fund = world
        .account_with("College fund", "education", "USD", json!({}))
        .await;
    world.balance(fund, BALANCE_ON, 50_000).await;
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
            &json!({"belongs_to": "household", "liquidity": "cash"}),
        )
        .await;

    let inside = world.forecast_at(ONE_MONTH).await;
    assert_eq!(minor(&inside, "goals"), minor(&leaving, "goals") + 50_000);
}

/// Neither balance shows a movement between them, so counted cash is right as it stands and
/// there is nothing to warn of.
#[tokio::test]
async fn a_movement_neither_balance_shows_raises_no_caution() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.charge(checking, AS_OF, 30_000, moved).await;
    let into = world.deposit(savings, AS_OF, 30_000).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&before, "spending_newer_than_balance")["total"],
        usd(30_000)
    );

    world.link_legs(&out, &into).await;

    assert_eq!(world.forecast_at(ONE_MONTH).await["cautions"], json!([]));
}

/// A difference is real money a balance is missing, so it alone stays asked about.
#[tokio::test]
async fn the_difference_of_a_movement_stays_newer_spending() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.charge(checking, AS_OF, 30_000, moved).await;
    let into = world.deposit(savings, AS_OF, 29_000).await;

    world.link_legs(&out, &into).await;

    let forecast = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&forecast, "spending_newer_than_balance")["total"],
        usd(1_000)
    );
}

/// A pending deposit may already be in savings' balance, and then cash is 300.00 too high with the
/// charge after checking's balance, so the alarm stands even if nothing turns out wrong.
#[tokio::test]
async fn a_pending_deposit_keeps_the_caution() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.charge(checking, AS_OF, 30_000, moved).await;
    let into = world.deposit(savings, AS_OF, 30_000).await;
    world.link_legs(&out, &into).await;
    world
        .server
        .store
        .execute_unchecked_for_tests("UPDATE transactions SET pending = 1 WHERE id = ?", &into)
        .await
        .unwrap();

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        caution(&forecast, "spending_newer_than_balance")["total"],
        usd(30_000)
    );
    assert_eq!(forecast["status"], "provisional");
}

/// The deposit shows and the charge doesn't: one balance is ahead of the other, and cash really
/// is overstated, so the alarm stands.
#[tokio::test]
async fn a_movement_only_one_balance_shows_keeps_the_caution() {
    let world = world().await;
    let (checking, savings) = world.two_accounts().await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.charge(checking, AS_OF, 30_000, moved).await;
    let into = world.deposit(savings, BALANCE_ON, 30_000).await;

    world.link_legs(&out, &into).await;

    assert_eq!(
        caution(
            &world.forecast_at(ONE_MONTH).await,
            "spending_newer_than_balance"
        )["total"],
        usd(30_000)
    );
}

/// A link to a closed account is no movement (T5), so a charge filed in a budget stays spending:
/// that account's cash isn't in the number, so the money did leave what is counted.
#[tokio::test]
async fn a_link_to_a_closed_account_changes_no_number() {
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
    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&before, "Groceries")["reduced_by"], usd(30_000));

    world.link_legs(&out, &into).await;

    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

impl World {
    /// 1,250.00 USD filed in a budget out of checking, and `euros_minor` EUR into a euro
    /// account, on the 5th, linked; returns the link.
    async fn dollars_to_euros(&self, euros_minor: i64) -> Value {
        let (checking, _) = self.two_accounts().await;
        let euros = self
            .account_with("Euros", "checking", "EUR", json!({}))
            .await;
        self.balance(euros, BALANCE_ON, 100_000).await;
        let food = self.expense("Food").await;
        self.item(&with(
            outflow("Groceries", 200_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
        let out = self.charge(checking, "2026-11-05", 125_000, food).await;
        let into = self.deposit(euros, "2026-11-05", euros_minor).await;
        self.link_legs(&out, &into).await
    }

    async fn enter_usd_rate(&self, on: &str, rate: &str) {
        self.admin_send(
            Method::PUT,
            &format!("/api/v1/fx/manual-rates/USD/{on}"),
            &json!({"rate": rate}),
        )
        .await;
    }
}

/// -1,250.00 USD linked to +12.50 EUR still moves all of both, as it always did; what changes is
/// that, once the day's rate is held, the forecast names the two legs and the rate.
#[tokio::test]
async fn a_link_across_currencies_that_disagrees_gets_a_caution_and_moves_the_same() {
    let world = world().await;
    let link = world.dollars_to_euros(1_250).await;
    let without_a_rate = world.forecast_at(ONE_MONTH).await;
    assert!(!kinds(&without_a_rate, "cautions").contains(&"transfer_link_rate_mismatch".into()));
    assert_eq!(line(&without_a_rate, "Groceries")["reduced_by"], usd(0));

    world.enter_usd_rate("2026-11-05", "1.1172").await;

    let with_a_rate = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&with_a_rate, "transfer_link_rate_mismatch"),
        &json!({
            "kind": "transfer_link_rate_mismatch",
            "link": link["id"],
            "outflow": usd(-125_000),
            "inflow": {"minor_units": 1_250, "currency": "EUR"},
            "expected_inflow": {"minor_units": 111_887, "currency": "EUR"},
            "rate_on": "2026-11-05",
            "manual_rate": true,
        })
    );
    assert_eq!(
        without_cautions(with_a_rate),
        without_cautions(without_a_rate),
        "a rate changes no number"
    );
}

/// The whole answer but its cautions.
fn without_cautions(mut forecast: Value) -> Value {
    forecast.as_object_mut().unwrap().remove("cautions");
    forecast
}

/// Pending amounts are in no count, so two of them have nothing to caution about; once one posts,
/// the link is asked about.
#[tokio::test]
async fn a_link_of_two_pending_legs_has_no_caution_until_one_posts() {
    let world = world().await;
    world.enter_usd_rate("2026-11-05", "1.1172").await;
    let link = world.dollars_to_euros(1_250).await;
    let legs = [
        link["outflow"]["transaction"].clone(),
        link["inflow"]["transaction"].clone(),
    ];
    for leg in &legs {
        world
            .server
            .store
            .execute_unchecked_for_tests(
                "UPDATE transactions SET pending = 1 WHERE id = ?",
                leg.as_str().unwrap(),
            )
            .await
            .unwrap();
    }
    let both_pending = world.forecast_at(ONE_MONTH).await;
    assert!(!kinds(&both_pending, "cautions").contains(&"transfer_link_rate_mismatch".into()));

    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE transactions SET pending = 0 WHERE id = ?",
            legs[0].as_str().unwrap(),
        )
        .await
        .unwrap();
    let one_posted = world.forecast_at(ONE_MONTH).await;
    assert!(kinds(&one_posted, "cautions").contains(&"transfer_link_rate_mismatch".into()));
}

/// A bank's spread isn't worth a caution.
#[tokio::test]
async fn a_link_across_currencies_inside_the_spread_has_no_caution() {
    let world = world().await;
    world.enter_usd_rate("2026-11-05", "1.25").await;

    // 1,250.00 USD is 1,000.00 EUR at 1.25; 960.00 is 4% less.
    world.dollars_to_euros(96_000).await;

    let forecast = world.forecast_at(ONE_MONTH).await;
    assert!(!kinds(&forecast, "cautions").contains(&"transfer_link_rate_mismatch".into()));
}

/// Last month's link is not this month's to ask about, as a leg taken back isn't.
#[tokio::test]
async fn a_link_with_both_legs_before_the_month_has_no_caution() {
    let world = world().await;
    world.enter_usd_rate("2026-10-05", "1.1172").await;
    let (checking, _) = world.two_accounts().await;
    let euros = world
        .account_with("Euros", "checking", "EUR", json!({}))
        .await;
    let out = world.deposit(checking, "2026-10-05", -125_000).await;
    let into = world.deposit(euros, "2026-10-05", 1_250).await;
    world.link_legs(&out, &into).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert!(!kinds(&forecast, "cautions").contains(&"transfer_link_rate_mismatch".into()));
}
