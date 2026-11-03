//! The check's arithmetic, with a rate of exactly 2 USD to the euro so every expected value
//! is worked out by hand.

use jiff::civil::{Date, date};

use super::{CrossCurrencyLink, RateCheck, RateMismatch, check_rate, mismatches};
use crate::fx::{RateBook, RateHistory, RateObservation};
use crate::ids::TransferLinkId;
use crate::money::{Currency, Money};

const DAY: Date = date(2026, 10, 30);

fn currency(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn money(minor_units: i64, code: &str) -> Money {
    Money {
        minor_units,
        currency: currency(code),
    }
}

fn book(code: &str, rates: &[(Date, &str)]) -> RateBook {
    let mut book = RateBook::default();
    book.insert(
        currency(code),
        RateHistory::new(
            rates
                .iter()
                .map(|&(on, text)| RateObservation {
                    on,
                    rate: text.parse().unwrap(),
                })
                .collect(),
        ),
    );
    book
}

/// 2 USD to the euro on the day.
fn two_to_one() -> RateBook {
    book("USD", &[(DAY, "2")])
}

/// 100.00 USD out, and `arrived` EUR minor units in. At the rate that is 50.00 EUR, so
/// 5% of it is 2.50.
fn hundred_dollars(arrived: i64) -> CrossCurrencyLink {
    CrossCurrencyLink {
        link: TransferLinkId::new(),
        outflow: money(-10_000, "USD"),
        inflow: money(arrived, "EUR"),
        outflow_on: DAY,
        inflow_on: DAY,
    }
}

fn agrees(link: &CrossCurrencyLink, rates: &RateBook) -> bool {
    check_rate(link, rates) == RateCheck::Agrees
}

#[test]
fn a_spread_inside_five_percent_agrees_from_both_sides() {
    let rates = two_to_one();
    for arrived in [5_000, 4_800, 5_200, 4_750, 5_250] {
        assert!(agrees(&hundred_dollars(arrived), &rates), "{arrived}");
    }
}

#[test]
fn one_unit_past_five_percent_is_a_mismatch_from_both_sides() {
    let rates = two_to_one();
    for arrived in [4_749, 5_251] {
        assert!(
            matches!(
                check_rate(&hundred_dollars(arrived), &rates),
                RateCheck::Mismatch(_)
            ),
            "{arrived}"
        );
    }
}

#[test]
fn a_mismatch_names_both_legs_and_the_rate() {
    let rates = two_to_one();
    let link = hundred_dollars(1_250);
    assert_eq!(
        check_rate(&link, &rates),
        RateCheck::Mismatch(RateMismatch {
            outflow: money(-10_000, "USD"),
            inflow: money(1_250, "EUR"),
            expected_inflow: money(5_000, "EUR"),
            rate_on: DAY,
            manual_rate: false,
        })
    );
}

#[test]
fn the_decimal_slip_that_motivates_the_check_is_caught() {
    // -1,250.00 USD against +12.50 EUR, at 1.1172 USD to the euro.
    let rates = book("USD", &[(DAY, "1.1172")]);
    let link = CrossCurrencyLink {
        link: TransferLinkId::new(),
        outflow: money(-125_000, "USD"),
        inflow: money(1_250, "EUR"),
        outflow_on: DAY,
        inflow_on: DAY,
    };
    assert!(matches!(check_rate(&link, &rates), RateCheck::Mismatch(_)));
}

#[test]
fn a_few_cents_never_differ_by_rounding_alone() {
    // 0.03 USD is 0.015 EUR, which rounds to 0.02 and 0.01 on either side.
    let rates = two_to_one();
    for arrived in [1, 2] {
        let link = CrossCurrencyLink {
            outflow: money(-3, "USD"),
            ..hundred_dollars(arrived)
        };
        assert!(agrees(&link, &rates), "{arrived}");
    }
}

#[test]
fn a_rate_an_admin_entered_is_used_and_named() {
    let mut rates = RateBook::default();
    rates.insert(
        currency("USD"),
        RateHistory::merged(
            vec![RateObservation {
                on: DAY,
                rate: "1".parse().unwrap(),
            }],
            vec![RateObservation {
                on: DAY,
                rate: "2".parse().unwrap(),
            }],
        ),
    );
    // At the published 1 USD to the euro, 4,000 is far off; at the manual 2 it is 20% off.
    match check_rate(&hundred_dollars(4_000), &rates) {
        RateCheck::Mismatch(found) => {
            assert!(found.manual_rate);
            assert_eq!(found.expected_inflow, money(5_000, "EUR"));
        }
        other => panic!("expected a mismatch, got {other:?}"),
    }
    assert!(agrees(&hundred_dollars(5_000), &rates));
}

#[test]
fn the_rate_is_carried_forward_from_an_earlier_day() {
    let rates = book("USD", &[(date(2026, 10, 28), "2")]);
    let link = hundred_dollars(1_250);
    match check_rate(&link, &rates) {
        RateCheck::Mismatch(found) => assert_eq!(found.rate_on, date(2026, 10, 28)),
        other => panic!("expected a mismatch, got {other:?}"),
    }
}

#[test]
fn the_later_posted_day_picks_the_rate() {
    let rates = book("USD", &[(date(2026, 10, 28), "2"), (DAY, "1")]);
    let link = CrossCurrencyLink {
        outflow_on: date(2026, 10, 28),
        ..hundred_dollars(5_000)
    };
    // The inflow posted on the 30th, at 1 USD to the euro: 100.00 EUR expected.
    assert!(matches!(check_rate(&link, &rates), RateCheck::Mismatch(_)));
}

#[test]
fn no_rate_on_or_before_the_day_is_not_checked() {
    let later = book("USD", &[(date(2026, 10, 31), "2")]);
    assert_eq!(
        check_rate(&hundred_dollars(1_250), &later),
        RateCheck::NotChecked
    );
    assert_eq!(
        check_rate(&hundred_dollars(1_250), &RateBook::default()),
        RateCheck::NotChecked
    );
}

#[test]
fn a_conversion_too_large_to_hold_is_not_checked() {
    let rates = book("JPY", &[(DAY, "175.68")]);
    let link = CrossCurrencyLink {
        link: TransferLinkId::new(),
        outflow: money(i64::MIN + 1, "JPY"),
        inflow: money(1, "EUR"),
        outflow_on: DAY,
        inflow_on: DAY,
    };
    // 9.2e18 yen is about 5.2e16 euro, which fits; the other way is what overflows.
    assert!(matches!(check_rate(&link, &rates), RateCheck::Mismatch(_)));
    let huge_euro = CrossCurrencyLink {
        outflow: money(i64::MIN + 1, "EUR"),
        inflow: money(1, "JPY"),
        ..link
    };
    assert_eq!(check_rate(&huge_euro, &rates), RateCheck::NotChecked);
}

#[test]
fn the_extremes_do_not_overflow() {
    let rates = book("USD", &[(DAY, "1")]);
    let link = CrossCurrencyLink {
        link: TransferLinkId::new(),
        outflow: money(i64::MIN + 1, "USD"),
        inflow: money(i64::MAX, "EUR"),
        outflow_on: DAY,
        inflow_on: DAY,
    };
    assert!(agrees(&link, &rates));
    let tiny = CrossCurrencyLink {
        inflow: money(1, "EUR"),
        ..link
    };
    assert!(matches!(check_rate(&tiny, &rates), RateCheck::Mismatch(_)));
}

#[test]
fn only_mismatching_links_are_named() {
    let rates = two_to_one();
    let (good, bad) = (hundred_dollars(5_000), hundred_dollars(1_250));
    let found = mismatches([&good, &bad], &rates);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].link, bad.link);
}
