//! R19: planned inflows count from the day after `as_of`. A balance dated `as_of` may
//! already hold that day's money, so counting it would count it twice. If it hasn't
//! arrived, the surplus reads low, the safe direction.

use jiff::civil::date;

use super::{
    AS_OF, cash, eur, has_line, household, hypothetical, inflow, line, monthly, one_off, run, usd,
    usd_totals,
};
use crate::forecast::{AccountInput, BalanceInput, ForecastInput, ForecastStatus, ItemInput};
use crate::ids::CategoryId;
use crate::money::Money;

/// One account holding 100,000,000 dated `as_of`, with income on.
fn with_income(items: Vec<ItemInput>) -> ForecastInput {
    ForecastInput {
        accounts: vec![AccountInput {
            balance: Some(BalanceInput {
                on: AS_OF,
                minor_units: 100_000_000,
            }),
            ..cash("Checking", usd(100_000_000))
        }],
        include_income: true,
        items,
        ..household()
    }
}

fn bonus_on_as_of() -> ItemInput {
    inflow(one_off("Bonus", usd(500_000), AS_OF, CategoryId::new()))
}

/// A salary paid on the 15th, the same day as `as_of`.
fn salary_on_as_of() -> ItemInput {
    inflow(monthly(
        "Salary",
        usd(800_000),
        date(2026, 1, 15),
        CategoryId::new(),
    ))
}

/// Each month's inflows in dollars, in order.
fn month_inflows(input: &ForecastInput) -> Vec<Money> {
    run(input)
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().inflows)
        .collect()
}

#[test]
fn an_inflow_on_as_of_is_not_counted_on_top_of_that_days_balance() {
    let forecast = run(&with_income(vec![bonus_on_as_of()]));
    assert_eq!(
        forecast.surplus,
        Some(usd(100_000_000)),
        "the balance alone"
    );
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing to warn of");
}

#[test]
fn an_inflow_the_day_after_as_of_counts() {
    let category = CategoryId::new();
    let forecast = run(&with_income(vec![inflow(one_off(
        "Bonus",
        usd(500_000),
        date(2026, 11, 16),
        category,
    ))]));
    assert_eq!(
        forecast.surplus,
        Some(usd(100_500_000)),
        "it is still to come"
    );
}

/// December 15 and January 15: February 15 is the window's end, which stays excluded.
#[test]
fn a_recurring_salary_on_as_of_counts_from_its_next_payday() {
    let forecast = run(&with_income(vec![salary_on_as_of()]));
    let salary = line(&forecast, "Salary");
    assert_eq!(salary.occurrences, 2, "December and January");
    assert_eq!(salary.first_on, Some(date(2026, 12, 15)), "not today");
    assert_eq!(salary.counted, usd(1_600_000), "two paydays");
}

#[test]
fn a_salary_on_as_of_adds_nothing_to_the_first_month() {
    assert_eq!(
        month_inflows(&with_income(vec![salary_on_as_of()])),
        [usd(0), usd(800_000), usd(800_000), usd(0)],
        "November's payday is in the balance"
    );
}

#[test]
fn income_left_out_does_not_report_an_inflow_on_as_of() {
    let forecast = run(&ForecastInput {
        include_income: false,
        ..with_income(vec![salary_on_as_of()])
    });
    assert_eq!(
        usd_totals(&forecast).excluded_inflows,
        usd(1_600_000),
        "only the paydays still to come"
    );
}

#[test]
fn a_hypothetical_inflow_on_as_of_is_not_counted() {
    let forecast = run(&with_income(vec![hypothetical(0, bonus_on_as_of())]));
    assert!(
        !has_line(&forecast, "Bonus"),
        "the same rule as a stored one"
    );
}

#[test]
fn a_foreign_inflow_only_on_as_of_is_no_second_currency() {
    let forecast = run(&with_income(vec![inflow(one_off(
        "Bonus",
        eur(500_000),
        AS_OF,
        CategoryId::new(),
    ))]));
    assert_eq!(forecast.blockers, [], "no euros are in use");
}

/// The day after the month's last day is the next bucket's.
#[test]
fn as_of_on_a_months_last_day_counts_the_next_days_inflow_in_the_next_month() {
    let last = date(2026, 11, 30);
    let input = ForecastInput {
        today: last,
        as_of: last,
        horizon_months: 1,
        stale_after_days: 365,
        ..with_income(vec![inflow(monthly(
            "Salary",
            usd(800_000),
            date(2026, 1, 1),
            CategoryId::new(),
        ))])
    };
    assert_eq!(
        month_inflows(&input),
        [usd(0), usd(800_000)],
        "December 1 lands in December"
    );
}
