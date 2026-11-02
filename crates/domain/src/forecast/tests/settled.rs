//! R9a: what a settled month does that the golden example and the router's tests don't
//! show. Worked by hand from the dates and amounts here.

use jiff::civil::date;

use super::{household, line, monthly, run, usd};
use crate::forecast::ForecastInput;
use crate::ids::CategoryId;
use crate::plan::{CalendarMonth, IntervalUnit, PlanSchedule, SettlementKind};

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

/// A bill every two weeks falls on 5 and 19 November 2026: as of the 15th the first has
/// passed and the second is to come, and the line lists both as one month's.
#[test]
fn a_month_settles_every_occurrence_it_holds_passed_or_not() {
    let mut item = monthly("Sitter", usd(1_000), date(2026, 1, 1), CategoryId::new());
    item.schedule = PlanSchedule::Recurring {
        every: 2,
        unit: IntervalUnit::Weeks,
        anchor_on: date(2026, 11, 5),
        end_on: None,
    };
    item.settled.insert(month("2026-11"), SettlementKind::Paid);
    let forecast = run(&ForecastInput {
        items: vec![item],
        horizon_months: 1,
        ..household()
    });

    let sitter = line(&forecast, "Sitter");
    assert_eq!(sitter.unpaid, usd(0), "the 5th owes nothing");
    assert_eq!(sitter.first_on, Some(date(2026, 12, 3)), "the 19th is gone");
    let [settled] = forecast.settled.as_slice() else {
        panic!("one settled line");
    };
    assert_eq!(settled.occurrences, 2, "both of November's");
    assert_eq!(settled.amount, usd(2_000), "their whole amount");
}

/// The window ends on 2027-02-15, before February's bill on the 20th: settling February
/// takes nothing out, so it isn't listed.
#[test]
fn a_month_the_window_only_touches_lists_only_what_it_reaches() {
    let mut item = monthly("Rent", usd(1_000), date(2026, 1, 20), CategoryId::new());
    item.settled
        .insert(month("2027-02"), SettlementKind::Skipped);
    let forecast = run(&ForecastInput {
        items: vec![item],
        ..household()
    });

    assert_eq!(forecast.settled, [], "nothing was taken out");
    assert_eq!(line(&forecast, "Rent").occurrences, 3, "Nov, Dec and Jan");
}
