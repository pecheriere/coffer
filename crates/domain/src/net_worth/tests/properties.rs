//! Random households: the answer must explain itself, whatever goes in. What is
//! expected comes from the input's rows and the rules, never from the report.

use std::collections::{BTreeMap, BTreeSet};

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, any, prop_assert, prop_assert_eq, prop_oneof, proptest};
use proptest::test_runner::TestCaseError;
use proptest::{collection, prop_compose};

use super::{Holding, currency, holding, id, input, rates};
use crate::accounts::LiquidityClass;
use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::net_worth::{
    DailyValue, NetWorthInput, NetWorthInterval, NetWorthReport, NetWorthStatus, PeriodEnd,
    ReportAccount, daily_values, net_worth_report, period_ends,
};

const MAX_AMOUNT: i64 = 1_000_000_000_000;
const MAX_HOLDINGS: u128 = 5;

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
    (0..=MAX_HOLDINGS).prop_flat_map(|count| (1..=count).map(random_holding).collect::<Vec<_>>())
}

fn interval() -> impl Strategy<Value = NetWorthInterval> {
    prop_oneof![
        Just(NetWorthInterval::Day),
        Just(NetWorthInterval::Week),
        Just(NetWorthInterval::Month),
    ]
}

/// N9, restated: closed, it counts before its closing day, or while it owes.
fn counts(line: &ReportAccount, row: &DailyValue) -> bool {
    line.closed_on
        .is_none_or(|closed_on| row.on < closed_on || row.balance_minor < 0)
}

/// What one period should answer, worked out from the input alone.
struct Expected<'a> {
    counted: Vec<(&'a ReportAccount, &'a DailyValue)>,
    /// Included, holding values by then, and with no row on the day.
    absent: Vec<&'a ReportAccount>,
}

fn expected(asked: &NetWorthInput, on: Date) -> Expected<'_> {
    let mut counted = Vec::new();
    let mut absent = Vec::new();
    for line in asked
        .accounts
        .iter()
        .filter(|line| line.include_in_net_worth)
    {
        match asked
            .rows
            .iter()
            .find(|row| row.account == line.id && row.on == on)
        {
            Some(row) if counts(line, row) => counted.push((line, row)),
            Some(_) => {}
            None => {
                let first = asked.first_on.iter().find(|(held, _)| *held == line.id);
                if first.is_some_and(|&(_, first_on)| first_on <= on) {
                    absent.push(line);
                }
            }
        }
    }
    Expected { counted, absent }
}

/// The amounts of `lines` in the display currency: `None` with any missing.
fn sum_in(
    asked: &NetWorthInput,
    lines: &[&(&ReportAccount, &DailyValue)],
    any_absent: bool,
) -> Option<Money> {
    let currency = asked.display.filter(|_| !any_absent)?;
    let minor_units = lines
        .iter()
        .map(|(_, row)| row.display_minor)
        .sum::<Option<i64>>()?;
    Some(Money {
        minor_units,
        currency,
    })
}

fn check_period(
    asked: &NetWorthInput,
    report: &NetWorthReport,
    period: &PeriodEnd,
) -> Result<(), TestCaseError> {
    let want = expected(asked, period.on);
    let point = report.points.iter().find(|point| point.on == period.on);
    let Some(point) = point else {
        prop_assert!(want.counted.is_empty() && want.absent.is_empty());
        return Ok(());
    };
    prop_assert!(!want.counted.is_empty() || !want.absent.is_empty());
    prop_assert_eq!(point.period_start, period.start);
    prop_assert_eq!(point.partial, period.partial);

    let mut natives: BTreeMap<Currency, i64> = BTreeMap::new();
    for (line, row) in &want.counted {
        *natives.entry(line.currency).or_default() += row.balance_minor;
    }
    let summed: Vec<(Currency, i64)> = point
        .by_currency
        .iter()
        .map(|sum| (sum.currency, sum.minor_units))
        .collect();
    prop_assert_eq!(summed, natives.into_iter().collect::<Vec<_>>());

    let all: Vec<_> = want.counted.iter().collect();
    prop_assert_eq!(point.total, sum_in(asked, &all, !want.absent.is_empty()));

    let in_lines: BTreeSet<&str> = point
        .by_liquidity
        .iter()
        .map(|line| line.liquidity.as_str())
        .collect();
    let in_input: BTreeSet<&str> = want
        .counted
        .iter()
        .map(|(line, _)| line.liquidity.as_str())
        .chain(want.absent.iter().map(|line| line.liquidity.as_str()))
        .collect();
    prop_assert_eq!(in_lines, in_input);
    for class_line in &point.by_liquidity {
        let of_class: Vec<_> = want
            .counted
            .iter()
            .filter(|(line, _)| line.liquidity == class_line.liquidity)
            .collect();
        let any_absent = want
            .absent
            .iter()
            .any(|line| line.liquidity == class_line.liquidity);
        prop_assert_eq!(class_line.amount, sum_in(asked, &of_class, any_absent));
    }
    if let Some(total) = point.total {
        let classes: Option<i64> = point
            .by_liquidity
            .iter()
            .map(|line| line.amount.map(|amount| amount.minor_units))
            .sum();
        prop_assert_eq!(classes, Some(total.minor_units));
    }

    let mut no_amount: BTreeSet<AccountId> = want.absent.iter().map(|line| line.id).collect();
    if asked.display.is_some() {
        no_amount.extend(
            want.counted
                .iter()
                .filter(|(_, row)| row.display_minor.is_none())
                .map(|(line, _)| line.id),
        );
    }
    let named: BTreeSet<AccountId> = point.unconverted.iter().copied().collect();
    prop_assert_eq!(named, no_amount);
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
        taken_out in collection::vec((1..=MAX_HOLDINGS, day()), 0..6),
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
                daily_values(
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
        asked.first_on = held
            .iter()
            .filter_map(|one| {
                let first = asked.rows.iter().find(|row| row.account == one.line.id)?;
                Some((one.line.id, first.on))
            })
            .collect();
        asked.rows.retain(|row| {
            !taken_out.iter().any(|&(n, on)| row.account == id(n) && row.on == on)
        });
        asked.today = window_end;
        asked.from = from;
        asked.to = from.checked_add(length.days()).unwrap();
        let through = from.checked_add(through_after_from.days()).unwrap();
        asked.through = Some(through);
        asked.interval = interval;
        let report = net_worth_report(&asked).unwrap();

        let end = asked.to.min(asked.today).min(through);
        let periods = if from <= end {
            period_ends(from, end, interval).unwrap()
        } else {
            Vec::new()
        };
        for period in &periods {
            check_period(&asked, &report, period)?;
        }
        let asked_days: BTreeSet<Date> = periods.iter().map(|period| period.on).collect();
        for point in &report.points {
            prop_assert!(asked_days.contains(&point.on));
        }
        for pair in report.points.windows(2) {
            prop_assert!(pair[0].on < pair[1].on);
        }
        prop_assert!(status_matches_lists(&report));
        prop_assert_eq!(report.accounts.len(), held.len());
    }
}
