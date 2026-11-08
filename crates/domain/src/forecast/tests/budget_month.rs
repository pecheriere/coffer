//! R9: a budget counts its whole calendar month. Its occurrences anywhere in `as_of`'s
//! month are what spending reduces, so one dated before `as_of` still needs what its
//! category has not spent. Later months count only inside the window.

use jiff::civil::{Date, date};

use super::{
    AS_OF, budget, cash, currency, eur, every, fixed_floor, has_line, household, hypothetical,
    line, monthly, one_off, paid_on, run, spent, usd,
};
use crate::forecast::{
    AccountInput, BalanceInput, Blocker, CategorySpent, Forecast, ForecastInput, ForecastStatus,
    ItemInput,
};
use crate::ids::CategoryId;
use crate::money::Money;
use crate::plan::{IntervalUnit, PlanSchedule};

/// Each month's outflows in dollars, in order.
fn outflows(forecast: &Forecast) -> Vec<Money> {
    forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().outflows)
        .collect()
}

fn until(item: ItemInput, anchor_on: Date, end_on: Date) -> ItemInput {
    ItemInput {
        schedule: PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on,
            end_on: Some(end_on),
        },
        ..item
    }
}

/// Cash of 2,000,000 dated today, a floor of 500,000 and three months from
/// 2026-11-15, up to 2027-02-15. `anchor_on` is a monthly budget of 80,000.
fn groceries(anchor_on: Date, spent_minor: i64) -> ForecastInput {
    let food = CategoryId::new();
    ForecastInput {
        accounts: vec![AccountInput {
            balance: Some(BalanceInput {
                on: AS_OF,
                minor_units: 2_000_000,
            }),
            ..cash("Checking", usd(2_000_000))
        }],
        floor: Some(fixed_floor(usd(500_000))),
        items: vec![budget(monthly("Groceries", usd(80_000), anchor_on, food))],
        category_spent: vec![spent(food, spent_minor)],
        ..household()
    }
}

const ON_THE_FIRST: Date = date(2026, 1, 1);

/// November's 80,000 less the 50,000 spent, then December, January and February.
#[test]
fn a_budget_dated_earlier_this_month_still_needs_what_is_unspent() {
    let forecast = run(&groceries(ON_THE_FIRST, 50_000));
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.gross, usd(320_000), "four months of 80,000");
    assert_eq!(groceries.reduced_by, usd(50_000), "what was spent");
    assert_eq!(groceries.counted, usd(270_000), "30,000 + 240,000");
}

/// 2,000,000 - 500,000 - 270,000.
#[test]
fn the_surplus_holds_back_what_is_unspent() {
    let forecast = run(&groceries(ON_THE_FIRST, 50_000));
    assert_eq!(forecast.surplus, Some(usd(1_230_000)), "the answer");
}

/// The same budget with nothing in November: only the 30,000 still to come differs.
#[test]
fn the_surplus_is_lower_by_exactly_what_is_unspent() {
    let from_december = run(&groceries(date(2026, 12, 1), 50_000));
    let from_earlier = run(&groceries(ON_THE_FIRST, 50_000));
    assert_eq!(
        from_december.surplus.unwrap().minor_units - from_earlier.surplus.unwrap().minor_units,
        30_000,
        "November's 80,000 less the 50,000 spent"
    );
}

#[test]
fn what_is_unspent_lands_in_the_first_month() {
    let forecast = run(&groceries(ON_THE_FIRST, 50_000));
    assert_eq!(
        outflows(&forecast),
        [usd(30_000), usd(80_000), usd(80_000), usd(80_000)],
        "November holds only what is left of its own"
    );
}

#[test]
fn the_line_names_the_occurrence_dated_earlier_this_month() {
    let forecast = run(&groceries(ON_THE_FIRST, 50_000));
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.occurrences, 4, "November to February");
    assert_eq!(groceries.first_on, Some(date(2026, 11, 1)), "before as_of");
    assert_eq!(groceries.last_on, Some(date(2027, 2, 1)), "in the window");
}

/// A budget can simply underspend: what it still needs is no gap to warn of.
#[test]
fn what_a_budget_still_needs_is_not_a_gap() {
    let forecast = run(&groceries(ON_THE_FIRST, 50_000));
    assert_eq!(line(&forecast, "Groceries").unpaid, usd(0), "not a gap");
    assert_eq!(forecast.cautions, [], "nothing to warn of");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
}

#[test]
fn a_budget_overspent_this_month_needs_nothing_more_this_month() {
    let forecast = run(&groceries(ON_THE_FIRST, 90_000));
    assert_eq!(
        line(&forecast, "Groceries").reduced_by,
        usd(80_000),
        "at most this month's amount"
    );
    assert_eq!(
        outflows(&forecast),
        [usd(0), usd(80_000), usd(80_000), usd(80_000)],
        "the overspending leaves later months whole"
    );
}

#[test]
fn a_budget_that_ended_earlier_this_month_counts_what_it_had_left() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![until(
            budget(monthly("Groceries", usd(80_000), ON_THE_FIRST, food)),
            ON_THE_FIRST,
            date(2026, 11, 10),
        )],
        category_spent: vec![spent(food, 50_000)],
        ..household()
    });
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.gross, usd(80_000), "November 1, before its end");
    assert_eq!(groceries.counted, usd(30_000), "less what was spent");
}

#[test]
fn an_occurrence_after_a_budgets_end_does_not_count() {
    let food = CategoryId::new();
    let anchor_on = date(2026, 1, 5);
    let forecast = run(&ForecastInput {
        items: vec![until(
            budget(monthly("Groceries", usd(80_000), anchor_on, food)),
            anchor_on,
            date(2026, 10, 31),
        )],
        ..household()
    });
    assert!(!has_line(&forecast, "Groceries"), "it ended in October");
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "nothing is held");
}

#[test]
fn an_occurrence_before_a_budgets_start_does_not_count() {
    let forecast = run(&groceries(date(2026, 12, 1), 50_000));
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.gross, usd(240_000), "December to February");
    assert_eq!(groceries.reduced_by, usd(0), "no November to reduce");
}

/// The window starts on the budget's own day, which must not count twice.
#[test]
fn as_of_on_the_first_counts_a_budget_on_the_first_once() {
    let first = date(2026, 11, 1);
    let forecast = run(&ForecastInput {
        today: first,
        as_of: first,
        ..groceries(ON_THE_FIRST, 50_000)
    });
    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.occurrences, 3, "November, December, January");
    assert_eq!(groceries.counted, usd(190_000), "240,000 less 50,000");
}

/// Mondays of 10,000: November 2, 9, 16, 23 and 30, and within the month's window
/// December 7 and 14.
fn weekly(spent_minor: i64) -> ForecastInput {
    let food = CategoryId::new();
    ForecastInput {
        horizon_months: 1,
        items: vec![budget(ItemInput {
            category: Some(food),
            ..super::stored(
                "Market",
                usd(10_000),
                every(1, IntervalUnit::Weeks, date(2026, 11, 2)),
            )
        })],
        category_spent: vec![spent(food, spent_minor)],
        ..household()
    }
}

#[test]
fn a_weekly_budget_counts_its_occurrences_either_side_of_as_of() {
    let forecast = run(&weekly(5_000));
    let market = line(&forecast, "Market");
    assert_eq!(market.occurrences, 7, "five in November, two in December");
    assert_eq!(market.gross, usd(70_000), "seven of 10,000");
    assert_eq!(market.counted, usd(65_000), "less the 5,000 spent");
}

#[test]
fn a_weekly_budgets_reduction_stays_in_the_first_month() {
    let forecast = run(&weekly(5_000));
    assert_eq!(
        outflows(&forecast),
        [usd(45_000), usd(20_000)],
        "November's five less 5,000, then December's two"
    );
}

/// 60,000 spent is more than November's 50,000, and December stays whole.
#[test]
fn a_weekly_budget_is_reduced_by_at_most_this_months_occurrences() {
    let forecast = run(&weekly(60_000));
    assert_eq!(line(&forecast, "Market").reduced_by, usd(50_000), "five");
}

#[test]
fn a_foreign_budget_earlier_this_month_counts_in_its_own_currency() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![budget(monthly(
            "Lebensmittel",
            eur(80_000),
            ON_THE_FIRST,
            food,
        ))],
        category_spent: vec![CategorySpent {
            category: food,
            currency: currency("EUR"),
            spent_minor: 50_000,
        }],
        ..household()
    });
    let lebensmittel = line(&forecast, "Lebensmittel");
    assert_eq!(lebensmittel.gross, eur(320_000), "four months of 80,000");
    assert_eq!(lebensmittel.counted, eur(270_000), "less the euros spent");
}

#[test]
fn dollars_spent_do_not_reduce_a_foreign_budget_earlier_this_month() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![budget(monthly(
            "Lebensmittel",
            eur(80_000),
            ON_THE_FIRST,
            food,
        ))],
        category_spent: vec![spent(food, 50_000)],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Lebensmittel").reduced_by,
        eur(0),
        "no euros"
    );
}

/// Its only occurrence left this month still puts the currency in use.
#[test]
fn a_foreign_budget_that_ended_earlier_this_month_is_a_second_currency() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![until(
            budget(monthly("Lebensmittel", eur(80_000), ON_THE_FIRST, food)),
            ON_THE_FIRST,
            date(2026, 11, 10),
        )],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::MixedCurrencies {
            currencies: vec![currency("EUR"), currency("USD")],
        }],
        "euros are needed and cannot be netted"
    );
}

/// Bills and one-offs still take their share first: 205,000 less the 200,000 rent.
#[test]
fn a_bills_share_comes_off_before_a_budget_earlier_this_month() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        items: vec![
            monthly("Rent", usd(200_000), ON_THE_FIRST, home),
            budget(monthly("Upkeep", usd(20_000), date(2026, 1, 5), home)),
        ],
        category_spent: vec![spent(home, 205_000)],
        spent_transactions: vec![
            paid_on(home, usd(200_000), date(2026, 11, 1)),
            paid_on(home, usd(5_000), date(2026, 11, 2)),
        ],
        ..household()
    });
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(5_000), "what the rent left");
    assert_eq!(upkeep.counted, usd(35_000), "November 5 and December 5");
}

/// An earlier budget no longer comes off the pool: it draws on it, earliest first.
#[test]
fn budgets_either_side_of_as_of_share_the_pool_earliest_first() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        items: vec![
            budget(monthly("Later", usd(10_000), date(2026, 1, 20), food)),
            budget(monthly("Earlier", usd(10_000), date(2026, 1, 5), food)),
        ],
        category_spent: vec![spent(food, 12_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Earlier").reduced_by, usd(10_000), "first");
    assert_eq!(line(&forecast, "Later").reduced_by, usd(2_000), "the rest");
}

#[test]
fn budgets_on_one_day_earlier_this_month_share_the_pool_in_input_order() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        items: vec![
            budget(monthly("First", usd(10_000), date(2026, 1, 5), food)),
            budget(monthly("Second", usd(10_000), date(2026, 1, 5), food)),
        ],
        category_spent: vec![spent(food, 15_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "First").reduced_by, usd(10_000), "first");
    assert_eq!(line(&forecast, "Second").reduced_by, usd(5_000), "the rest");
}

/// An imagined budget has no spending of its own to look back on.
#[test]
fn a_hypothetical_budget_earlier_this_month_stays_out() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![hypothetical(
            0,
            budget(monthly("What if", usd(10_000), ON_THE_FIRST, food)),
        )],
        ..household()
    });
    assert_eq!(
        line(&forecast, "What if").first_on,
        Some(date(2026, 12, 1)),
        "from the window only"
    );
}

/// A bill on the same day looks back only to pay what it owes, not to add to `gross`.
#[test]
fn a_bill_earlier_this_month_stays_out_of_gross() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Rent", usd(200_000), ON_THE_FIRST, home)],
        category_spent: vec![spent(home, 200_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Rent").gross, usd(600_000), "three months");
}

/// Rent of 200,000 on `as_of` and a budget of 20,000 on the 20th in one category, so the
/// window holds Nov, Dec and Jan of each, with `spent_minor` seen through `as_of`.
fn rent_due_today(spent_minor: i64) -> ForecastInput {
    let home = CategoryId::new();
    ForecastInput {
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 15), home),
            budget(monthly("Upkeep", usd(20_000), date(2026, 1, 20), home)),
        ],
        category_spent: vec![spent(home, spent_minor)],
        ..household()
    }
}

#[test]
fn a_bill_paid_on_as_of_does_not_eat_a_budget() {
    let forecast = run(&rent_due_today(200_000));
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(0), "the spending was the rent");
    assert_eq!(upkeep.counted, usd(60_000), "three months in full");
}

/// §10.8: the rent was paid the day before `as_of`, so the balance no longer holds it and
/// November's needs nothing: holding it again would count it twice. A bill is still never reduced:
/// what is left is December's and January's in full.
#[test]
fn a_payment_the_day_before_pays_that_days_bill() {
    let forecast = run(&rent_due_today(200_000));
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.reduced_by, usd(0), "a bill is never reduced");
    assert_eq!(rent.counted, usd(400_000), "December's and January's");
}

#[test]
fn spending_beyond_a_bill_on_as_of_reduces_a_budget() {
    let input = rent_due_today(205_000);
    let home = input.category_spent[0].category;
    let forecast = run(&ForecastInput {
        spent_transactions: vec![
            paid_on(home, usd(200_000), AS_OF),
            paid_on(home, usd(5_000), AS_OF),
        ],
        ..input
    });
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(5_000), "the rest");
}

/// 150,000 doesn't fit the rent, so it is spending on the budget, all 20,000 of this month's,
/// and the rent still counts in full.
#[test]
fn spending_that_fits_no_bill_on_as_of_reduces_a_budget() {
    let forecast = run(&rent_due_today(150_000));
    assert_eq!(
        line(&forecast, "Upkeep").reduced_by,
        usd(20_000),
        "all of it"
    );
    assert_eq!(line(&forecast, "Rent").counted, usd(600_000), "in full");
}

#[test]
fn a_bill_on_as_of_is_still_no_gap() {
    let forecast = run(&rent_due_today(150_000));
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0), "still to come");
    assert_eq!(forecast.cautions, [], "nothing to warn of");
}

#[test]
fn a_one_off_paid_on_as_of_does_not_eat_a_budget() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        items: vec![
            one_off("Tyres", usd(30_000), AS_OF, car),
            budget(monthly("Fuel", usd(20_000), date(2026, 1, 20), car)),
        ],
        category_spent: vec![spent(car, 30_000)],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Fuel").reduced_by,
        usd(0),
        "it was the tyres"
    );
}

#[test]
fn a_bill_the_day_after_as_of_takes_no_share_of_the_pool() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 16), home),
            budget(monthly("Upkeep", usd(20_000), date(2026, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 5_000)],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Upkeep").reduced_by,
        usd(5_000),
        "not due yet"
    );
}

/// Spending pays what has passed: the 100,000 pays the 10th's bill, and today's bill,
/// still to come, owes nothing.
#[test]
fn a_bill_on_as_of_leaves_spending_to_an_earlier_one() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            monthly("Today", usd(100_000), date(2026, 1, 15), home),
            monthly("Passed", usd(100_000), date(2026, 1, 10), home),
        ],
        category_spent: vec![spent(home, 100_000)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "no gap either way");
}
