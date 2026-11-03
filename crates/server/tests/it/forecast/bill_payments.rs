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
            "paid_by": {"transaction": paid, "posted_on": "2026-11-02", "amount": usd(1_000)},
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
