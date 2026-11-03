//! R9a: the note on a budget that spending lowered beside a bill settled as paid. Worked by hand
//! from the dates and amounts here: as of 15 November 2026, a month's window to 15 December, cash
//! 1,000,000 and a floor of 0, a bill on 5 November and a budget on the 20th.

use jiff::civil::date;

use super::bill_matching::{beside_a_budget, settled_beside_a_budget};
use super::usd;
use crate::forecast::{ForecastStatus, SettledBudget};
use crate::plan::{CalendarMonth, SettlementKind};

fn november() -> CalendarMonth {
    "2026-11".parse().unwrap()
}

/// Rent of 2,000 charged 2,300, which is past the 10% tolerance and fits nothing, beside a budget
/// of 500. Settled as paid, the rent's 2,000 comes off the spending and the 300 left lowers the
/// budget to a need of 200, where the truth, were all 2,300 the rent's, is 500. The forecast says
/// 300, stays firm, and no other number moves: cash 1,000,000 less 200 is 999,800.
#[test]
fn a_charge_past_the_tolerance_leaves_a_note_of_what_it_took() {
    let forecast = settled_beside_a_budget(
        usd(2_000),
        date(2026, 11, 5),
        500,
        &[(2_300, date(2026, 11, 5))],
    );

    assert_eq!(forecast.settled_budgets.len(), 1, "one budget");
    let SettledBudget {
        name,
        month,
        amount,
        ..
    } = &forecast.settled_budgets[0];
    assert_eq!(name, "Upkeep");
    assert_eq!(*month, november());
    assert_eq!(*amount, usd(300));
    assert_eq!(forecast.status, ForecastStatus::Firm, "a person decided");
    assert_eq!(forecast.cautions, []);
    assert_eq!(forecast.surplus, Some(usd(999_800)));
}

/// 230 on a bill of 200 beside a budget of 150: 200 comes off, the 30 left lowers the budget,
/// which needs 120. If all 230 was the bill's, that is 30 low, and the note says 30.
#[test]
fn the_note_names_only_what_lowered_the_budget() {
    let forecast = settled_beside_a_budget(
        usd(200),
        date(2026, 11, 5),
        150,
        &[(230, date(2026, 11, 5))],
    );
    assert_eq!(forecast.settled_budgets[0].amount, usd(30));
}

/// Paid in two parts of 1,000 against a bill of 2,000: all of it is the bill's, nothing lowers the
/// budget, which needs its 500, so there is nothing to say.
#[test]
fn no_note_when_the_settled_bill_took_all_the_spending() {
    let forecast = settled_beside_a_budget(
        usd(2_000),
        date(2026, 11, 5),
        500,
        &[(1_000, date(2026, 11, 5)), (1_000, date(2026, 11, 6))],
    );
    assert_eq!(forecast.settled_budgets, []);
}

/// 1,950 fits a bill of 2,000 and is its payment, so no assumption was made of the 100 spent on
/// the budget: nothing to say, and the budget needs 400.
#[test]
fn no_note_when_a_transaction_fits_the_settled_bill() {
    let forecast = settled_beside_a_budget(
        usd(2_000),
        date(2026, 11, 5),
        500,
        &[(1_950, date(2026, 11, 5)), (100, date(2026, 11, 8))],
    );
    assert_eq!(forecast.settled_budgets, []);
}

/// Skipped takes nothing from the category's spending, and an unsettled bill is matched by what
/// fits or counted as unpaid: neither assumes anything, so there is nothing to say.
#[test]
fn no_note_for_a_skipped_or_unsettled_bill() {
    for kind in [Some(SettlementKind::Skipped), None] {
        let forecast = beside_a_budget(
            kind,
            usd(2_000),
            date(2026, 11, 5),
            500,
            &[(2_300, date(2026, 11, 5))],
        );
        assert_eq!(forecast.settled_budgets, []);
    }
}
