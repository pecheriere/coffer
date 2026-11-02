//! R9: one spending pool per category and currency.

use jiff::civil::date;

use super::{AS_OF, cash, eur, fixed_floor, household, hypothetical, line, monthly, one_off, run};
use super::{currency, usd};
use crate::forecast::{CategorySpent, ForecastInput};
use crate::ids::CategoryId;

fn spent(category: CategoryId, minor: i64) -> CategorySpent {
    CategorySpent {
        category,
        currency: currency("USD"),
        spent_minor: minor,
    }
}

/// One month from the 15th, so the window holds only November's second half and
/// December's first.
fn one_month() -> ForecastInput {
    ForecastInput {
        horizon_months: 1,
        ..household()
    }
}

#[test]
fn one_spending_pool_per_category() {
    let bills = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Later", usd(10_000), date(2026, 1, 25), bills),
            monthly("Sooner", usd(10_000), date(2026, 1, 20), bills),
        ],
        category_spent: vec![spent(bills, 15_000)],
        ..one_month()
    });
    let sooner = line(&forecast, "Sooner");
    assert_eq!(sooner.reduced_by, usd(10_000), "the 20th goes first");
    assert_eq!(sooner.counted, usd(0), "wholly paid for");
    let later = line(&forecast, "Later");
    assert_eq!(later.reduced_by, usd(5_000), "what is left of the pool");
    assert_eq!(
        later.counted,
        usd(5_000),
        "the same dollars are not used twice"
    );
}

#[test]
fn items_on_one_day_take_from_the_pool_in_input_order() {
    let bills = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("First", usd(10_000), date(2026, 1, 20), bills),
            monthly("Second", usd(10_000), date(2026, 1, 20), bills),
        ],
        category_spent: vec![spent(bills, 15_000)],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "First").counted,
        usd(0),
        "first in the list"
    );
    assert_eq!(
        line(&forecast, "Second").counted,
        usd(5_000),
        "then the rest"
    );
}

#[test]
fn earlier_planned_spending_consumes_the_pool() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 1), home),
            monthly("Maintenance", usd(20_000), date(2026, 1, 20), home),
        ],
        category_spent: vec![spent(home, 200_000)],
        ..one_month()
    });
    let maintenance = line(&forecast, "Maintenance");
    assert_eq!(maintenance.reduced_by, usd(0), "the spending was the rent");
    assert_eq!(maintenance.counted, usd(20_000), "still to come in full");
}

#[test]
fn spending_beyond_earlier_plans_reduces_what_remains() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 1), home),
            monthly("Maintenance", usd(20_000), date(2026, 1, 20), home),
        ],
        category_spent: vec![spent(home, 205_000)],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Maintenance").reduced_by,
        usd(5_000),
        "205,000 spent less 200,000 planned"
    );
}

#[test]
fn an_earlier_one_off_consumes_the_pool() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            one_off("Tyres", usd(30_000), date(2026, 11, 5), car),
            monthly("Fuel", usd(20_000), date(2026, 1, 20), car),
        ],
        category_spent: vec![spent(car, 40_000)],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Fuel").reduced_by,
        usd(10_000),
        "40,000 spent less 30,000 planned"
    );
}

#[test]
fn an_occurrence_on_as_of_is_still_to_come() {
    let bills = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Today", usd(10_000), AS_OF, bills)],
        category_spent: vec![spent(bills, 4_000)],
        ..one_month()
    });
    let today = line(&forecast, "Today");
    assert_eq!(today.first_on, Some(AS_OF), "the window includes as_of");
    assert_eq!(today.reduced_by, usd(4_000), "it does not eat the pool");
}

#[test]
fn an_occurrence_on_the_first_of_the_month_consumes_the_pool() {
    let bills = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("First", usd(6_000), date(2026, 1, 1), bills),
            monthly("Later", usd(10_000), date(2026, 1, 20), bills),
        ],
        category_spent: vec![spent(bills, 10_000)],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Later").reduced_by,
        usd(4_000),
        "10,000 spent less the 6,000 on November 1"
    );
}

#[test]
fn one_offs_are_never_reduced() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Repair", usd(10_000), date(2026, 11, 20), car)],
        category_spent: vec![spent(car, 50_000)],
        ..household()
    });
    let repair = line(&forecast, "Repair");
    assert_eq!(repair.reduced_by, usd(0), "a one-off is never reduced");
    assert_eq!(repair.counted, usd(10_000), "counted in full");
}

#[test]
fn reduction_only_touches_the_current_month() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10_000), date(2026, 1, 20), food)],
        category_spent: vec![spent(food, 25_000)],
        ..household()
    });
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.gross, usd(30_000), "Nov, Dec, Jan");
    assert_eq!(groceries.reduced_by, usd(10_000), "November's only");
    assert_eq!(groceries.counted, usd(20_000), "December and January whole");
    let outflows: Vec<_> = forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().outflows)
        .collect();
    assert_eq!(
        outflows,
        [usd(0), usd(10_000), usd(10_000), usd(0)],
        "the reduction lands in the first bucket"
    );
}

#[test]
fn reduction_never_below_zero() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10_000), date(2026, 1, 20), food)],
        category_spent: vec![spent(food, 1_000_000)],
        ..one_month()
    });
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.reduced_by, usd(10_000), "at most the amount");
    assert_eq!(groceries.counted, usd(0), "never negative");
    assert_eq!(
        forecast.surplus,
        Some(usd(1_000_000)),
        "overspending does not add cash"
    );
}

#[test]
fn hypothetical_items_are_not_reduced() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![hypothetical(
            0,
            monthly("What if", usd(10_000), date(2026, 1, 20), food),
        )],
        category_spent: vec![spent(food, 5_000)],
        ..one_month()
    });
    let what_if = line(&forecast, "What if");
    assert_eq!(what_if.reduced_by, usd(0), "a what-if is not reduced");
    assert_eq!(what_if.counted, usd(10_000), "counted in full");
}

#[test]
fn hypothetical_items_do_not_consume_the_pool() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            hypothetical(0, monthly("What if", usd(30_000), date(2026, 1, 5), food)),
            hypothetical(1, monthly("Soon", usd(30_000), date(2026, 1, 16), food)),
            monthly("Groceries", usd(10_000), date(2026, 1, 20), food),
        ],
        category_spent: vec![spent(food, 10_000)],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Groceries").reduced_by,
        usd(10_000),
        "the stored item has the whole pool"
    );
}

#[test]
fn spending_in_another_currency_does_not_reduce() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10_000), date(2026, 1, 20), food)],
        category_spent: vec![CategorySpent {
            category: food,
            currency: currency("EUR"),
            spent_minor: 10_000,
        }],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Groceries").reduced_by,
        usd(0),
        "the pool is per category and currency"
    );
}

#[test]
fn pools_of_each_currency_stay_apart() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(100)), cash("Konto", eur(100))],
        floor: Some(fixed_floor(usd(0))),
        items: vec![
            monthly("Dollars", usd(10_000), date(2026, 1, 20), food),
            monthly("Euros", eur(10_000), date(2026, 1, 21), food),
        ],
        category_spent: vec![
            spent(food, 3_000),
            CategorySpent {
                category: food,
                currency: currency("EUR"),
                spent_minor: 4_000,
            },
        ],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Dollars").reduced_by, usd(3_000), "dollars");
    assert_eq!(line(&forecast, "Euros").reduced_by, eur(4_000), "euros");
}

#[test]
fn spending_in_another_category_does_not_reduce() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10_000), date(2026, 1, 20), food)],
        category_spent: vec![spent(CategoryId::new(), 10_000)],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Groceries").reduced_by, usd(0), "no match");
}

#[test]
fn goals_neither_reduce_nor_consume() {
    let home = CategoryId::new();
    let mut roof = super::goal("Roof", usd(50_000), date(2026, 1, 1), date(2026, 12, 1), 0);
    roof.category = Some(home);
    let forecast = run(&ForecastInput {
        items: vec![
            roof,
            monthly("Maintenance", usd(5_000), date(2026, 1, 20), home),
        ],
        category_spent: vec![spent(home, 2_000)],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Roof").reduced_by, usd(0), "goal untouched");
    assert_eq!(line(&forecast, "Roof").counted, usd(50_000), "goal in full");
    assert_eq!(
        line(&forecast, "Maintenance").reduced_by,
        usd(2_000),
        "the recurring item has the pool"
    );
}

#[test]
fn inflows_neither_reduce_nor_consume() {
    let mixed = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            super::inflow(monthly("Refund", usd(5_000), date(2026, 1, 3), mixed)),
            super::inflow(monthly("Pay", usd(5_000), date(2026, 1, 16), mixed)),
            monthly("Bill", usd(5_000), date(2026, 1, 20), mixed),
        ],
        category_spent: vec![spent(mixed, 2_000)],
        include_income: true,
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Pay").reduced_by,
        usd(0),
        "inflow untouched"
    );
    assert_eq!(line(&forecast, "Bill").reduced_by, usd(2_000), "whole pool");
}
