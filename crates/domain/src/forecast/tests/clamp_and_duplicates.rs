//! Exact answers around a clamped window, and what a possible duplicate can hide.

use jiff::civil::{Date, date};

use super::{assert_provisional, currency, inflow, line, monthly, one_off, run, usd, usd_totals};
use super::{budget, household};
use crate::forecast::{
    CategorySpent, Caution, ForecastInput, ForecastStatus, ItemInput, LooseEnds,
};
use crate::ids::CategoryId;
use crate::plan::{IntervalUnit, PlanSchedule, PlanSpending};

fn spent(category: CategoryId, minor: i64) -> CategorySpent {
    CategorySpent {
        category,
        currency: currency("USD"),
        spent_minor: minor,
    }
}

/// A firm household of 1,000,000 with `as_of` as today and a balance fresh enough
/// for any date these tests use.
fn on(as_of: Date, horizon_months: u8) -> ForecastInput {
    ForecastInput {
        today: as_of,
        as_of,
        horizon_months,
        stale_after_days: 365,
        ..household()
    }
}

/// Rent of 200,000 anchored on `anchor_on`, with `spent_minor` seen this month.
fn rent_in(as_of: Date, horizon_months: u8, anchor_on: Date, spent_minor: i64) -> ForecastInput {
    let home = CategoryId::new();
    ForecastInput {
        items: vec![monthly("Rent", usd(200_000), anchor_on, home)],
        category_spent: vec![spent(home, spent_minor)],
        ..on(as_of, horizon_months)
    }
}

/// The answer and the occurrences in the window, with this month's rent seen.
fn seen(as_of: Date, horizon_months: u8, anchor_on: Date) -> (Option<i64>, u32, ForecastStatus) {
    let forecast = run(&rent_in(as_of, horizon_months, anchor_on, 200_000));
    (
        forecast.surplus.map(|surplus| surplus.minor_units),
        line(&forecast, "Rent").occurrences,
        forecast.status,
    )
}

#[test]
fn rent_on_the_clamped_day_counts_exactly() {
    let firm = ForecastStatus::Firm;
    assert_eq!(
        seen(date(2027, 1, 30), 1, date(2026, 1, 28)),
        (Some(800_000), 1, firm),
        "February 28, the clamped day"
    );
    assert_eq!(
        seen(date(2026, 11, 30), 3, date(2026, 1, 28)),
        (Some(400_000), 3, firm),
        "December 28, January 28, and February 28, the clamped day"
    );
    assert_eq!(
        seen(date(2027, 3, 31), 3, date(2026, 1, 30)),
        (Some(400_000), 3, firm),
        "April 30, May 30, and June 30, the clamped day"
    );
}

#[test]
fn a_window_that_does_not_clamp_is_unchanged() {
    assert_eq!(
        seen(date(2027, 1, 20), 1, date(2026, 1, 19)),
        (Some(800_000), 1, ForecastStatus::Firm),
        "February 19 only"
    );
}

#[test]
fn unseen_rent_on_a_clamped_window_adds_its_gap() {
    let forecast = run(&rent_in(date(2027, 1, 30), 1, date(2026, 1, 28), 0));
    assert_eq!(
        forecast.surplus,
        Some(usd(600_000)),
        "February's 200,000 and January's unseen 200,000"
    );
    assert_eq!(line(&forecast, "Rent").unpaid, usd(200_000), "the gap");
    assert_provisional(&forecast);
}

fn salary(anchor_on: Date) -> ItemInput {
    inflow(monthly(
        "Salary",
        usd(800_000),
        anchor_on,
        CategoryId::new(),
    ))
}

#[test]
fn the_clamped_day_adds_no_income_in_one_month() {
    let forecast = run(&ForecastInput {
        include_income: true,
        items: vec![salary(date(2026, 1, 31))],
        ..on(date(2027, 1, 31), 1)
    });
    let line = line(&forecast, "Salary");
    assert_eq!(line.occurrences, 1, "January 31 only");
    assert_eq!(line.last_on, Some(date(2027, 1, 31)), "not February 28");
    assert_eq!(forecast.surplus, Some(usd(1_800_000)), "one salary");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
}

#[test]
fn the_clamped_day_adds_no_income_in_three_months() {
    let forecast = run(&ForecastInput {
        include_income: true,
        items: vec![salary(date(2026, 1, 31))],
        ..on(date(2026, 11, 30), 3)
    });
    let line = line(&forecast, "Salary");
    assert_eq!(line.occurrences, 3, "November 30, December 31, January 31");
    assert_eq!(line.last_on, Some(date(2027, 1, 31)), "not February 28");
    assert_eq!(forecast.surplus, Some(usd(3_400_000)), "three salaries");
}

#[test]
fn income_left_out_stops_at_the_same_end() {
    let forecast = run(&ForecastInput {
        items: vec![salary(date(2026, 1, 31))],
        ..on(date(2027, 1, 31), 1)
    });
    assert_eq!(
        usd_totals(&forecast).excluded_inflows,
        usd(800_000),
        "the income shown is the income that would count"
    );
    let february = forecast.months.last().unwrap().lines.first().unwrap();
    assert_eq!(february.inflows, usd(0), "none in February");
}

#[test]
fn the_same_window_still_counts_rent_on_the_clamped_day() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        include_income: true,
        items: vec![
            salary(date(2026, 1, 31)),
            monthly("Rent", usd(200_000), date(2026, 1, 28), home),
        ],
        category_spent: vec![spent(home, 200_000)],
        ..on(date(2027, 1, 31), 1)
    });
    assert_eq!(line(&forecast, "Rent").occurrences, 1, "February 28 counts");
    assert_eq!(line(&forecast, "Salary").occurrences, 1, "one salary");
    assert_eq!(
        forecast.surplus,
        Some(usd(1_600_000)),
        "1,000,000 + 800,000 - 200,000"
    );

    let forecast = run(&ForecastInput {
        include_income: true,
        items: vec![
            salary(date(2026, 1, 31)),
            monthly("Rent", usd(200_000), date(2026, 1, 28), home),
        ],
        category_spent: vec![spent(home, 200_000)],
        ..on(date(2026, 11, 30), 3)
    });
    assert_eq!(
        line(&forecast, "Rent").occurrences,
        3,
        "through February 28"
    );
    assert_eq!(line(&forecast, "Salary").occurrences, 3, "three salaries");
    assert_eq!(
        forecast.surplus,
        Some(usd(2_800_000)),
        "1,000,000 + 2,400,000 - 600,000"
    );
}

/// Bills that ended this month, so only the spending stands between them and a gap.
fn ended(name: &str, amount: i64, anchor_on: Date, category: CategoryId) -> ItemInput {
    ItemInput {
        schedule: PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on,
            end_on: Some(date(2026, 11, 10)),
        },
        ..monthly(name, usd(amount), anchor_on, category)
    }
}

fn loose(uncategorized_count: u32, possible_duplicates: u32) -> LooseEnds {
    LooseEnds {
        currency: currency("USD"),
        uncategorized_count,
        uncategorized_minor: 7_000,
        possible_duplicates,
    }
}

/// If one of the 2,000 spent is a duplicate, one bill has not been paid.
#[test]
fn a_possible_duplicate_can_hide_an_unpaid_bill() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            ended("A", 1_000, date(2026, 1, 1), home),
            ended("B", 1_000, date(2026, 1, 5), home),
        ],
        category_spent: vec![spent(home, 2_000)],
        loose_ends: vec![loose(0, 1)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::PossibleDuplicates { count: 1 }],
        "the spending that paid the bills may be wrong"
    );
    assert_provisional(&forecast);
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "the number stands");
}

#[test]
fn a_possible_duplicate_can_hide_an_unpaid_one_off() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Tyres", usd(1_000), date(2026, 11, 5), car)],
        category_spent: vec![spent(car, 1_000)],
        loose_ends: vec![loose(0, 1)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::PossibleDuplicates { count: 1 }],
        "the same for a one-off"
    );
}

/// More spending found cannot open a gap, so uncategorized money changes nothing.
#[test]
fn uncategorized_spending_cannot_open_a_gap() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![ended("A", 1_000, date(2026, 1, 1), home)],
        category_spent: vec![spent(home, 1_000)],
        loose_ends: vec![loose(3, 0)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [],
        "paid bills leave it nothing to change"
    );
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
}

#[test]
fn a_possible_duplicate_with_no_earlier_payment_changes_nothing() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            budget(monthly("Upkeep", usd(1_000), date(2026, 1, 5), home)),
            one_off("Later", usd(1_000), date(2026, 11, 20), home),
        ],
        category_spent: vec![spent(home, 2_000)],
        loose_ends: vec![loose(0, 1)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "no bill or one-off was paid by it");
}

fn marked(spending: Option<PlanSpending>, item: ItemInput) -> ItemInput {
    ItemInput { spending, ..item }
}

#[test]
fn a_one_off_marked_as_a_budget_is_never_reduced() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![marked(
            Some(PlanSpending::Budget),
            one_off("Repair", usd(10_000), date(2026, 11, 20), car),
        )],
        category_spent: vec![spent(car, 5_000)],
        ..household()
    });
    let repair = line(&forecast, "Repair");
    assert_eq!(
        repair.reduced_by,
        usd(0),
        "only a recurring item is a budget"
    );
    assert_eq!(repair.counted, usd(10_000), "counted in full");
}

#[test]
fn an_earlier_one_off_marked_as_a_budget_still_owes() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![marked(
            Some(PlanSpending::Budget),
            one_off("Repair", usd(10_000), date(2026, 11, 5), car),
        )],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Repair").unpaid,
        usd(10_000),
        "its gap counts"
    );
    assert_provisional(&forecast);
}

#[test]
fn a_recurring_outflow_with_no_spending_kind_is_a_bill() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![marked(
            None,
            monthly("Later", usd(10_000), date(2026, 1, 20), home),
        )],
        category_spent: vec![spent(home, 5_000)],
        ..household()
    });
    assert_eq!(line(&forecast, "Later").reduced_by, usd(0), "not reduced");

    let forecast = run(&ForecastInput {
        items: vec![marked(
            None,
            monthly("Earlier", usd(10_000), date(2026, 1, 10), home),
        )],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Earlier").unpaid,
        usd(10_000),
        "and it owes"
    );
    assert_provisional(&forecast);
}
