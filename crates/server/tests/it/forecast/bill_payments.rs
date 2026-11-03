//! Which transaction paid which occurrence, through the real router (spec §8.9 R9b): the numbers
//! worked by hand, with Checking at 10,000.00, no floor, and one month from 15 November 2026, so
//! to 15 December.

use axum::http::Method;
use serde_json::{Value, json};

use super::settled::holding;
use super::{BALANCE_ON, caution, kinds, line, monthly, noon, one_off_on, outflow, usd, with};

const ONE_MONTH: &str = "?horizon_months=1";

/// The `(on, posted_on)` of each entry of `payments` on the line called `name`, `null` for one
/// nothing paid.
fn paid_days(forecast: &Value, name: &str) -> Vec<(String, Value)> {
    line(forecast, name)["payments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|one| {
            (
                one["on"].as_str().unwrap().to_owned(),
                one["paid_by"]["posted_on"].clone(),
            )
        })
        .collect()
}

/// October's bill, paid on 2 November, is October's. November's, on the 28th, is still to come
/// and counts: 10,000.00 - 10.00. The answer names the transaction.
#[tokio::test]
async fn a_bill_paid_on_the_2nd_pays_last_month() {
    let (world, checking) = holding().await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-10-28")))
        .await;
    let paid = world
        .spend_linked(checking, "2026-11-02", 1_000, rent, &Value::Null)
        .await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    let line = line(&forecast, "Rent");
    assert_eq!(line["unpaid"], usd(0));
    assert_eq!(line["counted"], usd(1_000));
    assert_eq!(
        line["payments"],
        json!([{
            "on": "2026-10-28",
            "amount": usd(1_000),
            "before_history": false,
            "paid_by": {
                "transaction": paid,
                "posted_on": "2026-11-02",
                "amount": usd(1_000),
                "difference": usd(0),
            },
        }])
    );
    assert_eq!(forecast["surplus"], usd(999_000));
    assert_eq!(forecast["status"], "firm");
}

/// Unpaid, it counts on top of November's and is named: 10,000.00 - 10.00 - 10.00.
#[tokio::test]
async fn a_bill_unpaid_from_last_month_counts_and_is_named() {
    let (world, _) = holding().await;
    let rent = world.expense("Rent").await;
    let item = world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-10-28")))
        .await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(line(&forecast, "Rent")["unpaid"], usd(1_000));
    assert_eq!(forecast["surplus"], usd(998_000));
    assert_eq!(forecast["status"], "provisional");
    let said = caution(&forecast, "bill_unpaid_last_month");
    assert_eq!(said["item"]["id"], item);
    assert_eq!(said["on"], "2026-10-28");
    assert_eq!(said["amount"], usd(1_000));
    let months = forecast["months"].as_array().unwrap();
    assert_eq!(
        months[0]["lines"][0]["outflows"],
        usd(2_000),
        "last month's lands in the first month, with November 28's"
    );
}

/// Two months and one payment: it pays October, the older, so November's is what is owed.
/// 10,000.00 - 10.00 (5 November) - 10.00 (5 December).
#[tokio::test]
async fn two_months_of_a_bill_with_one_payment_owe_the_newer() {
    let (world, checking) = holding().await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-01-05")))
        .await;
    world.spend(checking, "2026-11-06", 1_000, rent).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        paid_days(&forecast, "Rent"),
        [
            ("2026-10-05".to_owned(), json!("2026-11-06")),
            ("2026-11-05".to_owned(), Value::Null),
        ]
    );
    assert_eq!(caution(&forecast, "bill_not_seen")["amount"], usd(1_000));
    assert_eq!(forecast["surplus"], usd(998_000));
}

/// A bill of 1,000.00 and a budget of 500.00 in one category, with 400.00 spent. That can't be the
/// bill, so the budget has it: 1,000.00 owed + 100.00 left of the budget + 1,000.00 on 5 December,
/// so 10,000.00 - 2,100.00. The exact amount then pays the bill and leaves the budget alone.
#[tokio::test]
async fn a_budget_and_a_bill_in_one_category_stay_apart() {
    let (world, checking) = holding().await;
    let home = world.expense("Home").await;
    world
        .item(&outflow("Mortgage", 100_000, home, &monthly("2026-11-05")))
        .await;
    world
        .item(&with(
            outflow("Upkeep", 50_000, home, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    world.spend(checking, "2026-11-06", 40_000, home).await;

    let spent = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&spent, "Mortgage")["unpaid"], usd(100_000));
    assert_eq!(line(&spent, "Upkeep")["reduced_by"], usd(40_000));
    assert_eq!(spent["surplus"], usd(790_000));

    world.spend(checking, "2026-11-07", 100_000, home).await;
    let paid = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&paid, "Mortgage")["unpaid"], usd(0));
    assert_eq!(line(&paid, "Upkeep")["reduced_by"], usd(40_000));
    assert_eq!(
        paid["surplus"],
        usd(890_000),
        "10,000.00 - 100.00 - 1,000.00"
    );
}

/// 95.00 is close to the bill of 100.00, but the budget of 400.00 could have spent it: said, with
/// the bill counted. Needed: 100.00 + 305.00 + 100.00. Settling as paid says it was the bill: the
/// budget is whole again, 400.00 + 100.00, and the same money doesn't lower the need twice.
#[tokio::test]
async fn a_transaction_that_could_be_either_is_said_until_settled() {
    let (world, checking) = holding().await;
    let home = world.expense("Home").await;
    let bill = world
        .item(&outflow("Internet", 10_000, home, &monthly("2026-11-05")))
        .await;
    world
        .item(&with(
            outflow("Upkeep", 40_000, home, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    let paid = world
        .spend_linked(checking, "2026-11-05", 9_500, home, &Value::Null)
        .await;

    let unclear = world.forecast_at(ONE_MONTH).await;
    let said = caution(&unclear, "bill_payment_unclear");
    assert_eq!(said["transaction"], paid);
    assert_eq!(said["transaction_amount"], usd(9_500));
    assert_eq!(unclear["surplus"], usd(949_500));

    world.settle(&bill, "2026-11", "paid").await;
    let settled = world.forecast_at(ONE_MONTH).await;
    assert_eq!(kinds(&settled, "cautions"), Vec::<String>::new());
    assert_eq!(line(&settled, "Upkeep")["reduced_by"], usd(0));
    assert_eq!(settled["surplus"], usd(950_000));
}

/// Rent of 2,000.00 charged 2,300.00 fits nothing, so settled as paid its 2,000.00 comes off the
/// spending and the 300.00 left lowers a budget of 500.00 to a need of 200.00, where the truth may
/// be 500.00. The answer says 300.00 for that budget and month, and stays firm: 10,000.00 less
/// 200.00 and December's rent of 2,000.00. Without the settlement there is no note.
#[tokio::test]
async fn a_budget_lowered_beside_a_settled_bill_is_noted_not_cautioned() {
    let (world, checking) = holding().await;
    let home = world.expense("Home").await;
    let rent = world
        .item(&outflow("Rent", 200_000, home, &monthly("2026-11-05")))
        .await;
    let upkeep = world
        .item(&with(
            outflow("Upkeep", 50_000, home, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    world.spend(checking, "2026-11-05", 230_000, home).await;
    assert_eq!(
        world.forecast_at(ONE_MONTH).await["settled_budgets"],
        json!([])
    );

    world.settle(&rent, "2026-11", "paid").await;
    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        forecast["settled_budgets"],
        json!([{
            "source": {"kind": "planned_item", "id": upkeep},
            "name": "Upkeep",
            "category": home,
            "month": "2026-11",
            "amount": usd(30_000),
        }])
    );
    assert_eq!(line(&forecast, "Upkeep")["counted"], usd(20_000));
    assert_eq!(forecast["status"], "firm");
    assert_eq!(forecast["cautions"], json!([]));
    assert_eq!(forecast["surplus"], usd(780_000));
}

/// Paid from an account that isn't counted, no money left counted cash, so it pays nothing and the
/// bill still counts, until a person settles it.
#[tokio::test]
async fn a_bill_paid_from_an_uncounted_account_stays_needed_until_settled() {
    let (world, _) = holding().await;
    let rent = world.expense("Rent").await;
    let bill = world
        .item(&outflow("Rent", 1_000, rent, &one_off_on("2026-10-20")))
        .await;
    let side = world
        .account_with(
            "Side",
            "checking",
            "USD",
            json!({"include_in_forecast": false}),
        )
        .await;
    world.balance(side, BALANCE_ON, 50_000).await;
    world.spend(side, "2026-10-20", 1_000, rent).await;

    let unpaid = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&unpaid, "Rent")["unpaid"], usd(1_000));
    assert_eq!(unpaid["surplus"], usd(999_000));
    assert_eq!(
        kinds(&unpaid, "cautions"),
        ["bill_unpaid_last_month".to_owned()]
    );

    world.settle(&bill, "2026-10", "paid").await;
    let settled = world.forecast_at(ONE_MONTH).await;
    assert_eq!(settled["surplus"], usd(1_000_000));
    assert_eq!(settled["status"], "firm");
    assert_eq!(settled["settled"][0]["month"], "2026-10");
}

/// A settled October takes no payment, so the one payment goes on to November: how a wrong match is
/// corrected. Only 5 December is left to count.
#[tokio::test]
async fn settling_a_month_corrects_a_match() {
    let (world, checking) = holding().await;
    let rent = world.expense("Rent").await;
    let item = world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-01-05")))
        .await;
    world.spend(checking, "2026-11-06", 1_000, rent).await;

    world.settle(&item, "2026-10", "skipped").await;
    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        paid_days(&forecast, "Rent"),
        [("2026-11-05".to_owned(), json!("2026-11-06"))]
    );
    assert_eq!(forecast["surplus"], usd(999_000));
    assert_eq!(forecast["status"], "firm");
}

/// A link between two counted accounts is one movement, which pays nothing. A link out of counted
/// cash is still a payment (spec §8.12 T6). Both in October, so the look-back reads links too.
#[tokio::test]
async fn a_link_pays_a_bill_only_if_money_left_counted_cash() {
    for (kind, unpaid) in [("checking", 1_000), ("brokerage", 0)] {
        let (world, checking) = holding().await;
        let rent = world.expense("Rent").await;
        world
            .item(&outflow("Rent", 1_000, rent, &one_off_on("2026-10-20")))
            .await;
        let other = world.account_with("Other", kind, "USD", json!({})).await;
        world.balance(other, BALANCE_ON, 50_000).await;
        let out = world
            .spend_linked(checking, "2026-10-20", 1_000, rent, &Value::Null)
            .await;
        let into = world.manual(other, "2026-10-20", 1_000).await;
        world
            .admin_send(
                Method::POST,
                "/api/v1/transfer-links",
                &json!({"outflow": out, "inflow": into}),
            )
            .await;

        let forecast = world.forecast_at(ONE_MONTH).await;

        assert_eq!(line(&forecast, "Rent")["unpaid"], usd(unpaid));
    }
}

/// A debit on the day before the 1st, across the edge of what the look-back's bills reach: 30
/// September pays 1 October, and 30 October pays 1 November, read through the store. Nothing is owed,
/// as of 10 November: 10,000.00 - 20.00 on 1 December, the window's only one.
#[tokio::test]
async fn a_debit_before_the_first_bill_looked_at_pays_it() {
    let (world, checking) = holding().await;
    world.server.clock.set(noon("2026-11-10"));
    world.balance(checking, "2026-11-09", 1_000_000).await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 2_000, rent, &monthly("2026-01-01")))
        .await;
    world.spend(checking, "2026-09-30", 2_000, rent).await;
    world.spend(checking, "2026-10-30", 2_000, rent).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        paid_days(&forecast, "Rent"),
        [
            ("2026-10-01".to_owned(), json!("2026-09-30")),
            ("2026-11-01".to_owned(), json!("2026-10-30")),
        ]
    );
    assert_eq!(forecast["surplus"], usd(998_000));
    assert_eq!(forecast["status"], "firm");
}

/// A weekly bill paid on its days, but for 1 October. The row of 24 September is that week's, so
/// though it is exactly 7 days before 1 October it doesn't pay it: 1 October takes the row of 8
/// October, and so on along, and one week of the seven is owed however they line up.
#[tokio::test]
async fn a_row_before_the_look_back_cannot_hide_a_missed_week() {
    let (world, checking) = holding().await;
    world.server.clock.set(noon("2026-11-10"));
    world.balance(checking, "2026-11-09", 1_000_000).await;
    let sitter = world.expense("Sitter").await;
    world
        .item(&outflow(
            "Sitter",
            1_000,
            sitter,
            &json!({"type": "recurring", "every": 1, "unit": "weeks", "anchor_on": "2026-09-24"}),
        ))
        .await;
    for on in [
        "2026-09-24",
        "2026-10-08",
        "2026-10-15",
        "2026-10-22",
        "2026-10-29",
        "2026-11-05",
    ] {
        world.spend(checking, on, 1_000, sitter).await;
    }

    let forecast = world.forecast_at(ONE_MONTH).await;

    let unpaid: Vec<String> = paid_days(&forecast, "Sitter")
        .into_iter()
        .filter(|(_, paid)| paid.is_null())
        .map(|(on, _)| on)
        .collect();
    assert_eq!(unpaid, ["2026-11-05"], "the last, by oldest first");
    assert_eq!(line(&forecast, "Sitter")["unpaid"], usd(1_000));
}

/// Eight bills of 750.00 and history that starts on 1 November: October's have no evidence, so the
/// surplus doesn't fall by 6,000.00 until December. One caution says how much, and November's, due
/// after history starts, are as ever.
#[tokio::test]
async fn bills_before_the_first_transaction_are_one_caution_and_no_hold() {
    let world = super::world().await;
    let checking = world.funded("Checking", 1_000_000).await;
    world.floor(0).await;
    let bills = world.expense("Bills").await;
    for name in ["A", "B", "C", "D", "E", "F", "G", "H"] {
        world
            .item(&outflow(name, 75_000, bills, &monthly("2026-10-20")))
            .await;
    }
    world.manual(checking, "2026-11-01", -1).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    let said = caution(&forecast, "bill_history_missing");
    assert_eq!(said["starts_on"], "2026-11-01");
    assert_eq!(said["occurrences"], 8);
    assert_eq!(said["amount"], usd(600_000));
    assert_eq!(line(&forecast, "A")["unpaid"], usd(0));
    assert_eq!(forecast["surplus"], usd(400_000), "only 20 November's");
    assert_eq!(
        kinds(&forecast, "cautions"),
        ["bill_history_missing".to_owned()]
    );
}

/// A rent really missed on 20 October reads unpaid. Closing the old account on 3 November and
/// starting a new one that day can't move the first day forward and drop it.
#[tokio::test]
async fn closing_an_account_does_not_let_off_a_missed_bill() {
    let world = super::world().await;
    let old = world.funded("Old", 0).await;
    world.manual(old, "2026-10-01", -1).await;
    world.floor(0).await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 200_000, rent, &one_off_on("2026-10-20")))
        .await;
    let new = world.funded("New", 1_000_000).await;
    world.manual(new, "2026-11-03", -1).await;
    world.close(old).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(line(&forecast, "Rent")["unpaid"], usd(200_000));
    assert_eq!(forecast["surplus"], usd(800_000));
    assert_eq!(
        caution(&forecast, "bill_unpaid_last_month")["amount"],
        usd(200_000)
    );
}

/// A household with only balances still answers for this month's bill.
#[tokio::test]
async fn a_balance_only_household_holds_this_months_bill() {
    let world = super::world().await;
    world.funded("Checking", 1_000_000).await;
    world.floor(0).await;
    let tyres = world.expense("Tyres").await;
    world
        .item(&outflow("Tyres", 100_000, tyres, &one_off_on("2026-11-05")))
        .await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(forecast["surplus"], usd(900_000));
    assert_eq!(kinds(&forecast, "cautions"), ["bill_not_seen"]);
}

/// October's uncategorized payment pays nothing yet, so a caution looks at October and says file it.
/// Filed, it pays the bill and both cautions go.
#[tokio::test]
async fn an_unfiled_payment_last_month_is_said_until_it_is_filed() {
    let (world, checking) = holding().await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &one_off_on("2026-10-20")))
        .await;
    let paid = world.manual(checking, "2026-10-20", -1_000).await;

    let unfiled = world.forecast_at(ONE_MONTH).await;
    let said = caution(&unfiled, "last_month_loose_ends");
    assert_eq!(said["uncategorized_count"], 1);
    assert_eq!(said["uncategorized_total"], usd(1_000));

    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/transactions/{}", paid.as_str().unwrap()),
            &json!({"splits": [{"category": rent, "minor_units": -1_000, "goal": null}]}),
        )
        .await;
    let filed = world.forecast_at(ONE_MONTH).await;
    assert_eq!(filed["status"], "firm");
    assert_eq!(line(&filed, "Rent")["unpaid"], usd(0));
}

/// A settled bill's money is no budget's, through the store: rent of 2,000.00 paid in two parts and
/// settled leaves a budget of 500.00 whole.
#[tokio::test]
async fn a_settled_bill_paid_in_two_parts_keeps_the_budget_whole() {
    let (world, checking) = holding().await;
    let home = world.expense("Home").await;
    let rent = world
        .item(&outflow("Rent", 200_000, home, &one_off_on("2026-11-05")))
        .await;
    world
        .item(&with(
            outflow("Upkeep", 50_000, home, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    world.spend(checking, "2026-11-05", 100_000, home).await;
    world.spend(checking, "2026-11-06", 100_000, home).await;

    world.settle(&rent, "2026-11", "paid").await;
    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(line(&forecast, "Upkeep")["counted"], usd(50_000));
    assert_eq!(forecast["surplus"], usd(950_000));
}

/// On the first of a month, all of last month is behind: 1 October paid on its day, and 1 November,
/// today, counts in full and owes nothing.
#[tokio::test]
async fn on_the_first_of_a_month_last_month_is_all_behind() {
    let (world, checking) = holding().await;
    world.server.clock.set(noon("2026-11-01"));
    world.balance(checking, "2026-10-31", 1_000_000).await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-01-01")))
        .await;
    world.spend(checking, "2026-10-01", 1_000, rent).await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        paid_days(&forecast, "Rent"),
        [
            ("2026-10-01".to_owned(), json!("2026-10-01")),
            ("2026-11-01".to_owned(), Value::Null),
        ]
    );
    let line = line(&forecast, "Rent");
    assert_eq!(line["unpaid"], usd(0));
    assert_eq!(line["counted"], usd(1_000), "1 November");
    assert_eq!(forecast["surplus"], usd(999_000));
    assert_eq!(forecast["status"], "firm");
}

/// On the last of a month, a month-end bill: 30 November, today, counts in full, and the
/// unpaid 31 October counts as well. 10,000.00 - 20.00, 31 December being outside the window's
/// 30 December end.
#[tokio::test]
async fn on_the_last_of_a_month_last_months_bill_still_counts() {
    let (world, checking) = holding().await;
    world.server.clock.set(noon("2026-11-30"));
    world.balance(checking, "2026-11-30", 1_000_000).await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-01-31")))
        .await;

    let forecast = world.forecast_at(ONE_MONTH).await;

    assert_eq!(
        paid_days(&forecast, "Rent"),
        [
            ("2026-10-31".to_owned(), Value::Null),
            ("2026-11-30".to_owned(), Value::Null),
        ]
    );
    assert_eq!(forecast["surplus"], usd(998_000));
    assert_eq!(
        kinds(&forecast, "cautions"),
        ["bill_unpaid_last_month".to_owned()]
    );
}

/// A what-if keeps what was paid and owed, and an imagined bill from last month owes nothing: only
/// its 5 December does. 10,000.00 - 10.00 unpaid - 10.00 stored - 50.00 imagined.
#[tokio::test]
async fn a_what_if_keeps_the_payments_and_looks_back_at_nothing_of_its_own() {
    let (world, _) = holding().await;
    let rent = world.expense("Rent").await;
    world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-10-28")))
        .await;

    let forecast = world
        .what_if(&json!({
            "horizon_months": 1,
            "items": [{
                "name": "Imagined",
                "category": rent,
                "amount": usd(5_000),
                "direction": "outflow",
                "schedule": monthly("2026-10-05"),
            }],
        }))
        .await;

    assert_eq!(line(&forecast, "Rent")["unpaid"], usd(1_000));
    let imagined = line(&forecast, "Imagined");
    assert_eq!(imagined["unpaid"], usd(0));
    assert_eq!(imagined["payments"], json!([]));
    assert_eq!(imagined["first_on"], "2026-12-05");
    assert_eq!(forecast["surplus"], usd(993_000));
}

/// The look-back runs from the household's day, read when asked: at 02:00 UTC on 1 November it is
/// still 31 October in Los Angeles, so September is last month there and 30 September still owes.
/// In UTC it is November, and September is out of reach.
#[tokio::test]
async fn the_look_back_runs_from_the_households_day() {
    let (world, checking) = holding().await;
    world
        .server
        .clock
        .set("2026-11-01T02:00:00Z".parse().unwrap());
    world.balance(checking, "2026-10-30", 1_000_000).await;
    let tyres = world.expense("Tyres").await;
    world
        .item(&outflow("Tyres", 1_000, tyres, &one_off_on("2026-09-30")))
        .await;

    let in_utc = world.forecast().await;
    assert_eq!(in_utc["as_of"], "2026-11-01");
    assert_eq!(in_utc["items"], json!([]));

    world
        .settings(&json!({"timezone": "America/Los_Angeles"}))
        .await;
    let in_los_angeles = world.forecast().await;
    assert_eq!(in_los_angeles["as_of"], "2026-10-31");
    assert_eq!(line(&in_los_angeles, "Tyres")["unpaid"], usd(1_000));
}
