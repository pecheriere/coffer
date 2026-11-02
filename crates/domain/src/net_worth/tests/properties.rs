//! Random households: the answer must explain itself, whatever goes in.

use std::collections::BTreeMap;

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, any, prop_assert, prop_assert_eq, prop_oneof, proptest};
use proptest::{collection, prop_compose};

use super::{Holding, currency, holding, input, rates};
use crate::accounts::LiquidityClass;
use crate::money::Currency;
use crate::net_worth::{NetWorthInterval, NetWorthReport, NetWorthStatus, net_worth_report};

const MAX_AMOUNT: i64 = 1_000_000_000_000;

/// A window with a leap day and several month ends in it.
fn day() -> impl Strategy<Value = Date> {
    (0_i32..60).prop_map(|offset| date(2028, 1, 15).checked_add(offset.days()).unwrap())
}

fn class() -> impl Strategy<Value = LiquidityClass> {
    prop_oneof![
        Just(LiquidityClass::Cash),
        Just(LiquidityClass::ShortTermLiability),
        Just(LiquidityClass::Invested),
        Just(LiquidityClass::Illiquid),
        Just(LiquidityClass::LongTermLiability),
    ]
}

fn code() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("EUR"),
        Just("USD"),
        Just("JPY"),
        Just("KWD"),
        Just("THB")
    ]
}

prop_compose! {
    fn random_holding(n: u128)(
        code in code(),
        liquidity in class(),
        included in any::<bool>(),
        closed_on in proptest::option::of(day()),
        snapshots in collection::vec((day(), -MAX_AMOUNT..MAX_AMOUNT), 0..5),
    ) -> Holding {
        let mut held = holding(n, code, liquidity, &snapshots);
        held.line.include_in_net_worth = included;
        held.line.closed_on = closed_on;
        held
    }
}

fn holdings() -> impl Strategy<Value = Vec<Holding>> {
    (0_usize..5).prop_flat_map(|count| {
        (0..count)
            .map(|n| random_holding(u128::try_from(n).unwrap() + 1))
            .collect::<Vec<_>>()
    })
}

fn interval() -> impl Strategy<Value = NetWorthInterval> {
    prop_oneof![
        Just(NetWorthInterval::Day),
        Just(NetWorthInterval::Week),
        Just(NetWorthInterval::Month),
    ]
}

/// Asserted over the points of a report made with account points.
fn explain(report: &NetWorthReport) -> Result<(), proptest::test_runner::TestCaseError> {
    for point in &report.points {
        let lines: Vec<_> = report
            .accounts
            .iter()
            .filter_map(|series| {
                let seen = series.points.iter().find(|seen| seen.on == point.on)?;
                seen.counted.then_some((series, seen))
            })
            .collect();
        prop_assert!(!lines.is_empty());

        let mut natives: BTreeMap<Currency, i64> = BTreeMap::new();
        for (series, seen) in &lines {
            prop_assert_eq!(seen.balance.currency, series.currency);
            *natives.entry(series.currency).or_default() += seen.balance.minor_units;
        }
        let summed: Vec<(Currency, i64)> = point
            .by_currency
            .iter()
            .map(|sum| (sum.currency, sum.minor_units))
            .collect();
        prop_assert_eq!(summed, natives.into_iter().collect::<Vec<_>>());

        let amounts: Option<Vec<i64>> = lines
            .iter()
            .map(|(_, seen)| seen.amount.map(|amount| amount.minor_units))
            .collect();
        match (point.total, amounts) {
            (Some(total), Some(amounts)) => {
                prop_assert_eq!(Some(total.currency), report.display_currency);
                prop_assert_eq!(total.minor_units, amounts.iter().sum::<i64>());
                let classes: Option<i64> = point
                    .by_liquidity
                    .iter()
                    .map(|line| line.amount.map(|amount| amount.minor_units))
                    .sum();
                prop_assert_eq!(classes, Some(total.minor_units));
            }
            (None, None) => {
                if report.display_currency.is_some() {
                    prop_assert!(!point.unconverted.is_empty());
                }
            }
            (total, _) => prop_assert!(false, "a total without its amounts: {:?}", total),
        }
        for line in &point.by_liquidity {
            let in_class: Vec<_> = lines
                .iter()
                .filter(|(series, _)| series.liquidity == line.liquidity)
                .collect();
            prop_assert!(!in_class.is_empty());
            let sum: Option<i64> = in_class
                .iter()
                .map(|(_, seen)| seen.amount.map(|amount| amount.minor_units))
                .sum();
            prop_assert_eq!(line.amount.map(|amount| amount.minor_units), sum);
        }
    }
    Ok(())
}

fn status_matches_lists(report: &NetWorthReport) -> bool {
    let expected = if !report.blockers.is_empty() {
        NetWorthStatus::Unavailable
    } else if report.cautions.is_empty() {
        NetWorthStatus::Firm
    } else {
        NetWorthStatus::Provisional
    };
    report.status == expected
}

proptest! {
    #[test]
    fn totals_explain_themselves(
        held in holdings(),
        display in proptest::option::of(code()),
        from in day(),
        length in 0_i32..60,
        through_after_from in -3_i32..60,
        interval in interval(),
    ) {
        let book = rates(&[
            ("USD", &[(date(2028, 1, 20), "1.1172"), (date(2028, 2, 29), "1.1188")]),
            ("JPY", &[(date(2028, 1, 1), "175.68")]),
            ("KWD", &[(date(2000, 1, 1), "0.34")]),
        ]);
        let mut asked = input(&held, display, &book);
        // The fixture's rows stop at its own today, before this window: redo them.
        let window_end = date(2028, 4, 30);
        asked.rows = held
            .iter()
            .flat_map(|one| {
                crate::net_worth::daily_values(
                    one.line.id,
                    one.line.currency,
                    &one.snapshots,
                    display.map(currency),
                    &book,
                    date(2028, 1, 1),
                    window_end,
                )
            })
            .collect();
        asked.today = window_end;
        asked.from = from;
        asked.to = from.checked_add(length.days()).unwrap();
        asked.through = Some(from.checked_add(through_after_from.days()).unwrap());
        asked.interval = interval;
        let report = net_worth_report(&asked).unwrap();

        explain(&report)?;
        prop_assert!(status_matches_lists(&report));
        prop_assert_eq!(report.accounts.len(), held.len());
        for point in &report.points {
            prop_assert!(point.on <= asked.to && point.on >= asked.from);
            prop_assert!(Some(point.on) <= asked.through);
        }
        for pair in report.points.windows(2) {
            prop_assert!(pair[0].on < pair[1].on);
        }
    }
}
