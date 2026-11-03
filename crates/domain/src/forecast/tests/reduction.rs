//! R9: one spending pool per category and currency.

use jiff::civil::date;

use super::{
    AS_OF, budget, cash, eur, fixed_floor, household, hypothetical, line, monthly, one_off,
    paid_on, run, run_as_given, spent,
};
use super::{assert_provisional, currency, usd};
use crate::forecast::{CategorySpent, ForecastInput, ForecastStatus, ItemInput};
use crate::ids::CategoryId;
use crate::plan::{IntervalUnit, PlanSchedule};

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
            budget(monthly("Later", usd(10_000), date(2026, 1, 25), bills)),
            budget(monthly("Sooner", usd(10_000), date(2026, 1, 20), bills)),
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
            budget(monthly("First", usd(10_000), date(2026, 1, 20), bills)),
            budget(monthly("Second", usd(10_000), date(2026, 1, 20), bills)),
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
            budget(monthly("Maintenance", usd(20_000), date(2026, 1, 20), home)),
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
            budget(monthly("Maintenance", usd(20_000), date(2026, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 205_000)],
        spent_transactions: vec![
            paid_on(home, usd(200_000), date(2026, 11, 1)),
            paid_on(home, usd(5_000), date(2026, 11, 3)),
        ],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Maintenance").reduced_by,
        usd(5_000),
        "205,000 spent less the 200,000 that paid the rent"
    );
}

#[test]
fn an_earlier_one_off_consumes_the_pool() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            one_off("Tyres", usd(30_000), date(2026, 11, 5), car),
            budget(monthly("Fuel", usd(20_000), date(2026, 1, 20), car)),
        ],
        category_spent: vec![spent(car, 40_000)],
        spent_transactions: vec![
            paid_on(car, usd(30_000), date(2026, 11, 5)),
            paid_on(car, usd(10_000), date(2026, 11, 6)),
        ],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Fuel").reduced_by,
        usd(10_000),
        "40,000 spent less the 30,000 that paid the tyres"
    );
}

#[test]
fn an_occurrence_on_as_of_is_still_to_come() {
    let bills = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![budget(monthly("Today", usd(10_000), AS_OF, bills))],
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
            budget(monthly("Later", usd(10_000), date(2026, 1, 20), bills)),
        ],
        category_spent: vec![spent(bills, 10_000)],
        spent_transactions: vec![
            paid_on(bills, usd(6_000), date(2026, 11, 1)),
            paid_on(bills, usd(4_000), date(2026, 11, 2)),
        ],
        ..one_month()
    });
    assert_eq!(
        line(&forecast, "Later").reduced_by,
        usd(4_000),
        "10,000 spent less the 6,000 that paid November 1's"
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
        items: vec![budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
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
        items: vec![budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
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
            budget(monthly("What if", usd(10_000), date(2026, 1, 20), food)),
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
            hypothetical(
                0,
                budget(monthly("What if", usd(30_000), date(2026, 1, 5), food)),
            ),
            hypothetical(
                1,
                budget(monthly("Soon", usd(30_000), date(2026, 1, 16), food)),
            ),
            budget(monthly("Groceries", usd(10_000), date(2026, 1, 20), food)),
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
        items: vec![budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
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
            budget(monthly("Dollars", usd(10_000), date(2026, 1, 20), food)),
            budget(monthly("Euros", eur(10_000), date(2026, 1, 21), food)),
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
        items: vec![budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
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
            budget(monthly("Maintenance", usd(5_000), date(2026, 1, 20), home)),
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
            budget(monthly("Bill", usd(5_000), date(2026, 1, 20), mixed)),
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

#[test]
fn a_bill_is_not_reduced_before_its_date() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Rent", usd(200_000), date(2026, 1, 20), home)],
        category_spent: vec![spent(home, 150_000)],
        ..one_month()
    });
    let rent = line(&forecast, "Rent");
    assert_eq!(
        rent.reduced_by,
        usd(0),
        "other spending does not pay a bill"
    );
    assert_eq!(rent.counted, usd(200_000), "it counts in full");
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing to warn of");
}

#[test]
fn a_bill_on_as_of_is_not_past_its_date() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Rent", usd(200_000), AS_OF, home)],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0), "still to come");
    assert_eq!(line(&forecast, "Rent").counted, usd(200_000), "once");
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing to warn of");
}

/// Rent on the 10th with `as_of` the 15th: Nov 10 has passed, and Dec 10, Jan 10
/// and Feb 10 are in the window.
fn rent_on_the_10th(home: CategoryId, spent_minor: i64) -> ForecastInput {
    ForecastInput {
        items: vec![monthly("Rent", usd(200_000), date(2026, 1, 10), home)],
        category_spent: vec![spent(home, spent_minor)],
        ..household()
    }
}

#[test]
fn an_unpaid_bill_counts_in_full_and_warns() {
    let home = CategoryId::new();
    let forecast = run(&rent_on_the_10th(home, 0));
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.gross, usd(600_000), "the window's three");
    assert_eq!(rent.unpaid, usd(200_000), "November's has not been seen");
    assert_eq!(rent.counted, usd(800_000), "so it still counts");
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(200_000))],
        "with the category and the amount"
    );
    assert_provisional(&forecast);
    assert_eq!(forecast.surplus, Some(usd(200_000)), "1,000,000 - 800,000");
    let november = forecast.months.first().unwrap().lines.first().unwrap();
    assert_eq!(
        november.outflows,
        usd(200_000),
        "it lands in the first month"
    );
    assert_eq!(
        november.projected_cash,
        usd(800_000),
        "cash has yet to fall"
    );
}

#[test]
fn a_bill_spent_in_full_adds_nothing() {
    let home = CategoryId::new();
    let forecast = run(&rent_on_the_10th(home, 200_000));
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0), "it has been seen");
    assert_eq!(
        line(&forecast, "Rent").counted,
        usd(600_000),
        "the window only"
    );
    assert_eq!(forecast.cautions, [], "no caution");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
    assert_eq!(forecast.surplus, Some(usd(400_000)), "no extra need");
}

/// 150,000 is too far from 200,000 to say it paid the rent. It may have been part of it, which a
/// person can settle.
#[test]
fn a_transaction_well_short_of_a_bill_pays_none_of_it() {
    let home = CategoryId::new();
    let forecast = run(&rent_on_the_10th(home, 150_000));
    assert_eq!(line(&forecast, "Rent").unpaid, usd(200_000), "all of it");
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(200_000))],
        "the whole bill"
    );
    assert_eq!(forecast.surplus, Some(usd(200_000)), "1,000,000 - 800,000");
}

#[test]
fn a_budget_can_simply_underspend() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![budget(monthly(
            "Groceries",
            usd(60_000),
            date(2026, 1, 10),
            food,
        ))],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Groceries").unpaid,
        usd(0),
        "no gap to carry"
    );
    assert_eq!(forecast.cautions, [], "no caution");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
}

/// A payment goes to the earliest bill it fits: 100,000 pays the one on the 1st.
#[test]
fn unpaid_bills_are_traced_in_date_order() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Second bill", usd(100_000), date(2026, 1, 10), home),
            monthly("First bill", usd(100_000), date(2026, 1, 1), home),
        ],
        category_spent: vec![spent(home, 100_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "First bill").unpaid, usd(0), "paid first");
    assert_eq!(
        line(&forecast, "Second bill").unpaid,
        usd(100_000),
        "the other"
    );
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(100_000))],
        "one caution per category"
    );
}

/// Rent 100,000 on the 1st and a budget of 50,000 on the 5th, with 100,000 spent:
/// the spending goes to the bill first, and the budget just underspent.
#[test]
fn a_budgets_underspending_is_not_a_bills_gap() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Rent", usd(100_000), date(2026, 1, 1), home),
            budget(monthly("Upkeep", usd(50_000), date(2026, 1, 5), home)),
        ],
        category_spent: vec![spent(home, 100_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0), "covered");
    assert_eq!(forecast.cautions, [], "no caution");
}

#[test]
fn a_bill_that_ended_before_the_window_still_counts_its_gap() {
    let home = CategoryId::new();
    let mut last = monthly("Last rent", usd(200_000), date(2026, 1, 10), home);
    last.schedule = PlanSchedule::Recurring {
        every: 1,
        unit: IntervalUnit::Months,
        anchor_on: date(2026, 1, 10),
        end_on: Some(date(2026, 11, 10)),
    };
    let forecast = run(&ForecastInput {
        items: vec![last],
        ..household()
    });
    let line = line(&forecast, "Last rent");
    assert_eq!(line.occurrences, 0, "none in the window");
    assert_eq!(line.gross, usd(0), "so no gross");
    assert_eq!(line.counted, usd(200_000), "but the gap counts");
    assert_eq!(forecast.surplus, Some(usd(800_000)), "taken off the cash");
}

#[test]
fn a_foreign_bills_gap_is_a_second_currency() {
    let home = CategoryId::new();
    let mut ended = monthly("Miete", eur(1_000), date(2026, 1, 10), home);
    ended.schedule = PlanSchedule::Recurring {
        every: 1,
        unit: IntervalUnit::Months,
        anchor_on: date(2026, 1, 10),
        end_on: Some(date(2026, 11, 10)),
    };
    let forecast = run(&ForecastInput {
        items: vec![ended],
        ..household()
    });
    assert_eq!(
        forecast.surplus, None,
        "euros are needed and cannot be netted"
    );
    assert_eq!(forecast.totals.len(), 2, "both currencies have a row");
}

/// A one-off is a specific expected payment, so nothing about it underspends.
#[test]
fn an_earlier_one_off_counts_its_gap_and_warns() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Tyres", usd(30_000), date(2026, 11, 10), car)],
        ..household()
    });
    let tyres = line(&forecast, "Tyres");
    assert_eq!(tyres.occurrences, 0, "its date is before the window");
    assert_eq!(tyres.gross, usd(0), "so no gross");
    assert_eq!(tyres.unpaid, usd(30_000), "nothing was seen against it");
    assert_eq!(tyres.counted, usd(30_000), "so it still counts");
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, car, usd(30_000))],
        "the same caution as a bill"
    );
    assert_provisional(&forecast);
    assert_eq!(forecast.surplus, Some(usd(970_000)), "1,000,000 - 30,000");
    let november = forecast.months.first().unwrap().lines.first().unwrap();
    assert_eq!(
        november.outflows,
        usd(30_000),
        "it lands in the first month"
    );
}

#[test]
fn an_earlier_one_off_spent_in_full_adds_nothing() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Tyres", usd(30_000), date(2026, 11, 10), car)],
        category_spent: vec![spent(car, 30_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Tyres").counted, usd(0), "only shown");
    assert_eq!(forecast.cautions, [], "no caution");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "no extra need");
}

#[test]
fn a_transaction_well_short_of_a_one_off_pays_none_of_it() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Tyres", usd(30_000), date(2026, 11, 10), car)],
        category_spent: vec![spent(car, 20_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Tyres").unpaid, usd(30_000), "all of it");
    assert_eq!(forecast.surplus, Some(usd(970_000)), "1,000,000 - 30,000");
}

/// A payment goes to bills and one-offs alike, earliest first: 100 pays the one on the 5th,
/// whichever kind it is, and leaves the one on the 10th.
#[test]
fn one_offs_and_bills_are_paid_in_date_order() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Rent", usd(100), date(2026, 1, 10), home),
            one_off("Boiler", usd(100), date(2026, 11, 5), home),
        ],
        category_spent: vec![spent(home, 100)],
        ..household()
    });
    assert_eq!(line(&forecast, "Boiler").counted, usd(0), "the earlier");
    assert_eq!(line(&forecast, "Rent").unpaid, usd(100), "then the later");
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(100))],
        "one caution per category"
    );
}

#[test]
fn an_earlier_one_off_reaches_back_to_the_first_of_last_month() {
    let car = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("This month", usd(100), date(2026, 11, 1), car),
            one_off("Last month", usd(1_000), date(2026, 10, 1), car),
            one_off("Too long ago", usd(5_000), date(2026, 9, 30), car),
            one_off("On as_of", usd(10_000), AS_OF, car),
        ],
        ..household()
    });
    assert_eq!(line(&forecast, "This month").unpaid, usd(100), "this month");
    assert_eq!(line(&forecast, "Last month").unpaid, usd(1_000), "last");
    assert!(
        !forecast
            .items
            .iter()
            .any(|line| line.name == "Too long ago"),
        "a one-off before last month stays out"
    );
    let on_as_of = line(&forecast, "On as_of");
    assert_eq!(on_as_of.unpaid, usd(0), "still to come");
    assert_eq!(on_as_of.counted, usd(10_000), "and counted once");
    assert_eq!(forecast.surplus, Some(usd(988_900)), "1,000,000 - 11,100");
}

#[test]
fn what_can_never_be_unpaid() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            hypothetical(0, monthly("What if", usd(1_000), date(2026, 1, 10), other)),
            hypothetical(
                1,
                one_off("What if once", usd(1_000), date(2026, 11, 10), other),
            ),
            super::inflow(monthly("Pay", usd(1_000), date(2026, 1, 10), other)),
            super::inflow(one_off("Refund", usd(1_000), date(2026, 11, 10), other)),
            budget(monthly("Upkeep", usd(1_000), date(2026, 1, 10), other)),
            ItemInput {
                category: Some(other),
                ..super::goal("Fund", usd(1_000), date(2026, 1, 1), date(2027, 6, 1), 0)
            },
        ],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "none of these owes anything");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
    for item in &forecast.items {
        assert_eq!(item.unpaid, usd(0), "no gap on any line");
    }
}

/// Rent of 2,000 on the 1st and a budget of 300 on the 20th in one category.
fn bill_and_budget(home: CategoryId, spent_minor: i64) -> ForecastInput {
    ForecastInput {
        items: vec![
            monthly("Rent", usd(2_000), date(2026, 1, 1), home),
            budget(monthly("Upkeep", usd(300), date(2026, 1, 20), home)),
        ],
        category_spent: vec![spent(home, spent_minor)],
        ..one_month()
    }
}

#[test]
fn a_paid_bill_does_not_eat_a_budget() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        spent_transactions: vec![
            paid_on(home, usd(2_000), date(2026, 11, 1)),
            paid_on(home, usd(100), date(2026, 11, 2)),
        ],
        ..bill_and_budget(home, 2_100)
    });
    assert_eq!(
        line(&forecast, "Rent").unpaid,
        usd(0),
        "the bill is covered"
    );
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(
        upkeep.reduced_by,
        usd(100),
        "2,100 spent less the 2,000 bill"
    );
    assert_eq!(upkeep.counted, usd(200), "leaving 200 needed");
    assert_eq!(forecast.cautions, [], "nothing to warn of");
}

/// 1,500 can't be told to be the 2,000 rent, so the rent is still owed in full and the money is
/// the budget's, which it more than empties. 2,000 is needed, where calling it part of the rent
/// would have said 800.
#[test]
fn money_that_fits_no_bill_goes_to_the_budget() {
    let home = CategoryId::new();
    let forecast = run(&bill_and_budget(home, 1_500));
    assert_eq!(line(&forecast, "Rent").unpaid, usd(2_000), "all of it");
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(300), "all the pool can take");
    assert_eq!(upkeep.counted, usd(0), "the budget is spent");
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(2_000))],
        "the bill is reported"
    );
}
