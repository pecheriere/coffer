//! R4: what this month's unsettled spending and a dip below the emergency fund raise, which keeps the
//! number but stops it being firm.

use jiff::civil::date;

use super::{assert_provisional, cash, fixed_fund, household, inflow};
use super::{currency, eur, monthly, months_fund, one_off, run, usd};
use crate::forecast::{
    CategorySpent, Caution, ForecastInput, ForecastStatus, ItemInput, LooseEnds,
};
use crate::ids::CategoryId;
use crate::plan::{CalendarMonth, IntervalUnit, PlanSchedule};

fn loose(uncategorized_count: u32, possible_duplicates: u32) -> LooseEnds {
    LooseEnds {
        currency: currency("USD"),
        uncategorized_count,
        uncategorized_minor: 7_000,
        possible_duplicates,
    }
}

#[test]
fn uncategorized_spending_warns_only_with_current_month_recurring() {
    let food = CategoryId::new();
    let warned = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![loose(3, 2)],
        ..household()
    });
    assert_eq!(
        warned.cautions,
        [
            Caution::UncategorizedSpending {
                currency: currency("USD"),
                count: 3,
                total: usd(7_000),
            },
            Caution::PossibleDuplicates { count: 2 },
        ],
        "either could yet change this month's reduction"
    );
    assert_provisional(&warned);

    for items in [
        Vec::new(),
        vec![super::budget(monthly(
            "Next month",
            usd(1),
            date(2026, 12, 20),
            food,
        ))],
        vec![super::budget(ItemInput {
            schedule: PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on: date(2026, 1, 10),
                end_on: Some(date(2026, 10, 31)),
            },
            ..monthly("Over last month", usd(1), date(2026, 1, 10), food)
        })],
        vec![monthly("A bill to come", usd(1), date(2026, 1, 20), food)],
        vec![one_off("One-off", usd(1), date(2026, 11, 20), food)],
        vec![inflow(monthly("Pay", usd(1), date(2026, 1, 20), food))],
        vec![super::hypothetical(
            0,
            super::budget(monthly("What if", usd(1), date(2026, 1, 20), food)),
        )],
    ] {
        let forecast = run(&ForecastInput {
            items,
            loose_ends: vec![loose(3, 2)],
            ..household()
        });
        assert_eq!(forecast.cautions, [], "the number could not change");
        assert_eq!(forecast.status, ForecastStatus::Firm, "so it is firm");
    }
}

/// A budget dated earlier this month still counts what is unspent, so more spending found
/// could lower the number.
#[test]
fn uncategorized_spending_warns_with_a_budget_earlier_this_month() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 1),
            food,
        ))],
        loose_ends: vec![loose(3, 0)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::UncategorizedSpending {
            currency: currency("USD"),
            count: 3,
            total: usd(7_000),
        }],
        "it could yet reduce this month's budget"
    );
}

/// Categorizing that spending could close the gap, so here it can change the number.
#[test]
fn uncategorized_spending_warns_with_an_unpaid_bill() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Rent", usd(10_000), date(2026, 1, 10), home)],
        loose_ends: vec![loose(3, 0)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [
            super::not_seen(&forecast, home, usd(10_000)),
            Caution::UncategorizedSpending {
                currency: currency("USD"),
                count: 3,
                total: usd(7_000),
            },
        ],
        "the gap, and what might explain it"
    );
    assert_provisional(&forecast);
}

#[test]
fn loose_ends_of_another_currency_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![LooseEnds {
            currency: currency("EUR"),
            ..loose(1, 1)
        }],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "pools are per currency");
}

#[test]
fn settled_loose_ends_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![loose(0, 0)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "nothing is loose");
}

/// Cash 1,000,000 with an emergency fund of 500,000. A bill of 700,000 on Nov 20 leaves
/// 300,000 at November's end; pay of 800,000 only arrives on Dec 1, and a second
/// bill of 1,000,000 on Jan 2 leaves 100,000 at January's end.
fn dipping(include_income: bool) -> ForecastInput {
    let other = CategoryId::new();
    ForecastInput {
        include_income,
        emergency_funds: fixed_fund(usd(500_000)),
        items: vec![
            one_off("Bill", usd(700_000), date(2026, 11, 20), other),
            inflow(one_off("Pay", usd(800_000), date(2026, 12, 1), other)),
            one_off("Bigger", usd(1_000_000), date(2027, 1, 2), other),
        ],
        ..household()
    }
}

#[test]
fn cash_dip_warns_only_with_income() {
    let forecast = run(&dipping(true));
    assert_eq!(
        forecast.cautions,
        [Caution::CashDipsBelowEmergencyFund {
            month: "2026-11".parse::<CalendarMonth>().unwrap(),
            currency: currency("USD"),
            projected_cash: usd(300_000),
        }],
        "the first month only: income in December cannot pay November's bill"
    );
    assert_provisional(&forecast);

    let forecast = run(&dipping(false));
    assert_eq!(forecast.cautions, [], "without income, no dip is reported");
    assert_eq!(forecast.status, ForecastStatus::Firm, "and it is firm");
}

#[test]
fn cash_exactly_on_the_emergency_fund_is_no_dip() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        include_income: true,
        emergency_funds: fixed_fund(usd(300_000)),
        items: vec![one_off("Bill", usd(700_000), date(2026, 11, 20), other)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "300,000 is not below 300,000");
}

#[test]
fn a_dip_reads_each_currencys_own_emergency_fund() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        include_income: true,
        accounts: vec![cash("Checking", usd(1_000)), cash("Konto", eur(1_000))],
        emergency_funds: months_fund(1),
        items: vec![
            one_off("Rechnung", eur(5_000), date(2026, 12, 1), other),
            monthly("Miete", eur(100), date(2026, 1, 20), other),
        ],
        ..household()
    });
    let dips: Vec<_> = forecast
        .cautions
        .iter()
        .filter_map(|caution| match caution {
            Caution::CashDipsBelowEmergencyFund {
                month,
                currency,
                projected_cash,
            } => Some((month.to_string(), *currency, *projected_cash)),
            _ => None,
        })
        .collect();
    assert_eq!(
        dips,
        [("2026-12".to_owned(), currency("EUR"), eur(-4_200))],
        "900 after November, then less 5,100 in December; dollars stay clear"
    );
    let totals = super::totals_of(&forecast, "EUR");
    assert_eq!(
        totals.emergency_fund,
        eur(100),
        "only Nov 20 is within one month"
    );
}

#[test]
fn spent_categories_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10), date(2026, 1, 20), food)],
        category_spent: vec![CategorySpent {
            category: food,
            currency: currency("USD"),
            spent_minor: 1,
        }],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "a reduction is not a caution");
}
