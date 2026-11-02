//! Known spending newer than a balance warns and leaves the number alone (R10a).

use super::{currency, household, monthly, run, usd};
use crate::forecast::{Caution, ForecastStatus, NewerSpending};
use crate::ids::CategoryId;

fn newer(code: &str, total_minor: i64) -> NewerSpending {
    NewerSpending {
        currency: currency(code),
        total_minor,
    }
}

#[test]
fn newer_spending_is_a_caution_and_keeps_the_number() {
    let mut input = household();
    let without = run(&input).surplus;
    input.newer_spending = vec![newer("USD", 1_700_000)];

    let forecast = run(&input);

    assert_eq!(forecast.status, ForecastStatus::Provisional, "not firm");
    assert_eq!(forecast.surplus, without, "the number does not move");
    assert_eq!(
        forecast.cautions,
        [Caution::SpendingNewerThanBalance {
            currency: currency("USD"),
            total: usd(1_700_000),
        }],
        "it names the currency and the total"
    );
}

#[test]
fn no_newer_spending_stays_firm() {
    let forecast = run(&household());
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing to warn of");
    assert_eq!(forecast.cautions, [], "no caution");
}

#[test]
fn a_total_of_zero_warns_of_nothing() {
    let mut input = household();
    input.newer_spending = vec![newer("USD", 0)];
    assert_eq!(run(&input).cautions, [], "no caution");
}

#[test]
fn each_currency_has_its_own_caution_and_adds_no_row() {
    let mut input = household();
    input.newer_spending = vec![newer("EUR", 10), newer("USD", 20)];

    let forecast = run(&input);

    assert_eq!(forecast.cautions.len(), 2, "one per currency");
    assert_eq!(
        forecast.totals.len(),
        1,
        "a warning puts no currency in use"
    );
    assert_eq!(forecast.blockers, [], "and no blocker");
}

/// A bill past its date with nothing seen still counts as unpaid: spending the balance
/// doesn't show pays no bill, and warning about it changes nothing there.
#[test]
fn newer_spending_leaves_an_unpaid_bill_unpaid() {
    let mut input = household();
    input.items = vec![monthly(
        "Rent",
        usd(200_000),
        jiff::civil::date(2026, 1, 1),
        CategoryId::new(),
    )];
    let without = run(&input);
    input.newer_spending = vec![newer("USD", 200_000)];

    let forecast = run(&input);

    assert_eq!(forecast.surplus, without.surplus, "the gap still counts");
    assert_eq!(forecast.totals, without.totals, "and so do the totals");
    assert!(
        forecast
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::BillNotSeen { .. })),
        "the bill still has its caution"
    );
}
