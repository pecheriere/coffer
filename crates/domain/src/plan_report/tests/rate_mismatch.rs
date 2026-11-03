//! T8: a link across currencies whose legs disagree at the day's rate is named, for the
//! reported months only, and no amount of the report changes.

use jiff::civil::{Date, date};

use super::{dollars, eur, input, report, usd};
use crate::fx::{RateBook, RateHistory, RateObservation};
use crate::ids::TransferLinkId;
use crate::money::Money;
use crate::plan_report::{PlanReportInput, ReportCaution};
use crate::transfer_rate::CrossCurrencyLink;

fn two_dollars_to_the_euro() -> RateBook {
    let mut rates = RateBook::default();
    rates.insert(
        usd(),
        RateHistory::new(
            [date(2025, 12, 19), date(2025, 12, 30)]
                .map(|on| RateObservation {
                    on,
                    rate: "2".parse().unwrap(),
                })
                .to_vec(),
        ),
    );
    rates
}

/// 100.00 USD out and `arrived` EUR minor units in, the outflow on `out_on` and the inflow
/// on `in_on`.
fn link(out_on: Date, in_on: Date, arrived: i64) -> CrossCurrencyLink {
    CrossCurrencyLink {
        link: TransferLinkId::new(),
        outflow: dollars(-10_000),
        inflow: Money {
            minor_units: arrived,
            currency: eur(),
        },
        outflow_on: out_on,
        inflow_on: in_on,
    }
}

fn asked(links: Vec<CrossCurrencyLink>) -> PlanReportInput {
    PlanReportInput {
        average_months: 2,
        cross_currency_links: links,
        rates: two_dollars_to_the_euro(),
        ..input(1)
    }
}

#[test]
fn a_link_that_disagrees_in_a_reported_month_is_named_with_its_legs_and_rate() {
    let bad = link(date(2026, 1, 2), date(2026, 1, 3), 1_250);
    let answer = report(&asked(vec![bad]));

    let [
        ReportCaution::TransferLinkRateMismatch {
            link,
            outflow,
            inflow,
            expected_inflow,
            rate_on,
            manual_rate,
        },
    ] = answer.cautions.as_slice()
    else {
        panic!("one caution, got {:?}", answer.cautions);
    };
    assert_eq!(*link, bad.link);
    assert_eq!(*outflow, bad.outflow);
    assert_eq!(*inflow, bad.inflow);
    assert_eq!(
        *expected_inflow,
        Money {
            minor_units: 5_000,
            currency: eur()
        }
    );
    assert_eq!(*rate_on, date(2025, 12, 30));
    assert!(!manual_rate);
}

#[test]
fn a_link_that_agrees_or_cannot_be_checked_says_nothing() {
    let fine = link(date(2026, 1, 2), date(2026, 1, 3), 5_000);
    let mut unchecked = asked(vec![link(date(2026, 1, 2), date(2026, 1, 3), 1_250)]);
    unchecked.rates = RateBook::default();

    assert_eq!(report(&asked(vec![fine])).cautions, []);
    assert_eq!(report(&unchecked).cautions, []);
}

#[test]
fn a_leg_in_the_averaging_month_alone_is_not_this_reports_to_name() {
    // December is read for the average and isn't reported.
    let december = link(date(2025, 12, 20), date(2025, 12, 21), 1_250);
    assert_eq!(report(&asked(vec![december])).cautions, []);
}

#[test]
fn either_leg_in_a_reported_month_is_enough() {
    let across = link(date(2025, 12, 31), date(2026, 1, 2), 1_250);
    assert_eq!(report(&asked(vec![across])).cautions.len(), 1);
}
