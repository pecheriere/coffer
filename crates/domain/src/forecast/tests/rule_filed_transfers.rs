//! Transfers a rule filed with no link are reported and change nothing else.

use jiff::civil::date;

use super::{currency, household, monthly, run, usd};
use crate::forecast::{Forecast, ForecastStatus, RuleFiledTransfers};
use crate::ids::CategoryId;
use crate::money::Money;

fn filed(code: &str, count: u32, minor: i64) -> RuleFiledTransfers {
    RuleFiledTransfers {
        currency: currency(code),
        count,
        amount: Money {
            minor_units: minor,
            currency: currency(code),
        },
    }
}

/// The forecast with its report taken out, to compare everything else.
fn without_report(mut forecast: Forecast) -> Forecast {
    forecast.rule_filed_transfers.clear();
    forecast
}

#[test]
fn a_rule_filed_transfer_is_reported_and_nothing_else_moves() {
    let mut input = household();
    input.items = vec![monthly(
        "Rent",
        usd(120_000),
        date(2026, 11, 28),
        CategoryId::new(),
    )];
    let without = run(&input);
    input.rule_filed_transfers = vec![filed("USD", 2, 45_000)];

    let with = run(&input);

    assert_eq!(with.rule_filed_transfers, [filed("USD", 2, 45_000)]);
    assert!(without.rule_filed_transfers.is_empty(), "none without");
    assert_eq!(
        without_report(with),
        without,
        "the whole forecast is the same but for the report"
    );
}

#[test]
fn it_never_makes_a_firm_forecast_provisional() {
    let mut input = household();
    input.rule_filed_transfers = vec![filed("USD", 1, 100)];

    assert_eq!(run(&input).status, ForecastStatus::Firm);
}

#[test]
fn a_currency_with_none_is_left_out_and_the_rest_sorted() {
    let mut input = household();
    input.rule_filed_transfers = vec![
        filed("USD", 1, 100),
        filed("EUR", 0, 0),
        filed("CAD", 3, 900),
    ];

    let forecast = run(&input);

    assert_eq!(
        forecast.rule_filed_transfers,
        [filed("CAD", 3, 900), filed("USD", 1, 100)]
    );
}
