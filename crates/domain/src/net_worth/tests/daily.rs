//! One account's values, day by day.

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::collection;
use proptest::prelude::{Just, Strategy, prop_assert, prop_assert_eq, prop_oneof, proptest};

use super::{currency, id, rates, snapshot};
use crate::accounts::BalanceSource;
use crate::fx::RateBook;
use crate::net_worth::{ConversionState, DailyValue, MIN_VALUE_DATE, SnapshotPoint, daily_values};

fn usd_rows(snapshots: &[SnapshotPoint], from: Date, through: Date) -> Vec<DailyValue> {
    let usd = currency("USD");
    daily_values(
        id(1),
        usd,
        snapshots,
        Some(usd),
        &RateBook::default(),
        from,
        through,
    )
}

#[test]
fn one_row_per_day_from_the_first_snapshot() {
    let rows = usd_rows(
        &[
            snapshot(date(2028, 2, 28), 100),
            snapshot(date(2028, 3, 1), 300),
        ],
        date(2028, 2, 1),
        date(2028, 3, 2),
    );
    let seen: Vec<(Date, i64)> = rows.iter().map(|row| (row.on, row.balance_minor)).collect();
    assert_eq!(
        seen,
        [
            (date(2028, 2, 28), 100),
            (date(2028, 2, 29), 100),
            (date(2028, 3, 1), 300),
            (date(2028, 3, 2), 300),
        ]
    );
}

#[test]
fn row_names_its_snapshot() {
    let manual = SnapshotPoint {
        on: date(2026, 10, 28),
        balance_minor: 700,
        source: BalanceSource::Manual,
    };
    let rows = usd_rows(&[manual], date(2026, 10, 30), date(2026, 10, 30));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].on, date(2026, 10, 30));
    assert_eq!(rows[0].balance_on, date(2026, 10, 28));
    assert_eq!(rows[0].balance_source, BalanceSource::Manual);
    assert_eq!(rows[0].balance_minor, 700);
}

#[test]
fn rows_start_no_earlier_than_1970() {
    let ancient = date(1, 1, 1);
    let rows = usd_rows(&[snapshot(ancient, 5)], ancient, date(1970, 1, 2));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].on, MIN_VALUE_DATE);
    assert_eq!(rows[0].balance_on, ancient);
}

#[test]
fn a_snapshot_after_through_makes_no_row() {
    let rows = usd_rows(
        &[snapshot(date(2026, 11, 3), 5)],
        date(2026, 11, 1),
        date(2026, 11, 2),
    );
    assert_eq!(rows, []);
}

#[test]
fn no_display_currency_keeps_the_balance() {
    let usd = currency("USD");
    let rows = daily_values(
        id(1),
        usd,
        &[snapshot(date(2026, 10, 30), 5)],
        None,
        &RateBook::default(),
        date(2026, 10, 30),
        date(2026, 10, 30),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].balance_minor, 5);
    assert_eq!(rows[0].display, None);
    assert_eq!(rows[0].display_minor, None);
    assert_eq!(rows[0].rate_on, None);
    assert_eq!(rows[0].conversion, ConversionState::NoDisplayCurrency);
}

#[test]
fn the_display_currency_itself_is_the_same_amount() {
    let rows = usd_rows(
        &[snapshot(date(2026, 10, 30), 5)],
        date(2026, 10, 30),
        date(2026, 10, 30),
    );
    assert_eq!(rows[0].display, Some(currency("USD")));
    assert_eq!(rows[0].display_minor, Some(5));
    assert_eq!(rows[0].rate_on, None);
    assert_eq!(rows[0].conversion, ConversionState::Same);
}

#[test]
fn each_day_is_converted_at_its_own_rate() {
    let friday = date(2026, 10, 30);
    let thursday = date(2026, 10, 29);
    let book = rates(&[("USD", &[(thursday, "1.113"), (friday, "1.1172")])]);
    let rows = daily_values(
        id(1),
        currency("EUR"),
        &[snapshot(thursday, 100_000)],
        Some(currency("USD")),
        &book,
        thursday,
        date(2026, 10, 31),
    );
    let seen: Vec<(Option<i64>, Option<Date>)> = rows
        .iter()
        .map(|row| (row.display_minor, row.rate_on))
        .collect();
    assert_eq!(
        seen,
        [
            (Some(111_300), Some(thursday)),
            (Some(111_720), Some(friday)),
            (Some(111_720), Some(friday)),
        ]
    );
    assert_eq!(rows[0].conversion, ConversionState::Converted);
}

#[test]
fn a_missing_rate_is_a_state_with_no_amount() {
    let on = date(2026, 10, 30);
    let rows = daily_values(
        id(1),
        currency("THB"),
        &[snapshot(on, 5)],
        Some(currency("USD")),
        &RateBook::default(),
        on,
        on,
    );
    assert_eq!(rows[0].display, Some(currency("USD")));
    assert_eq!(rows[0].display_minor, None);
    assert_eq!(rows[0].rate_on, None);
    assert_eq!(rows[0].conversion, ConversionState::Missing);
}

#[test]
fn too_large_is_a_state_not_a_failure() {
    let on = date(2026, 10, 30);
    let book = rates(&[("KWD", &[(on, "9999999999")])]);
    let rows = daily_values(
        id(1),
        currency("EUR"),
        &[snapshot(on, i64::MAX)],
        Some(currency("KWD")),
        &book,
        on,
        on,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].balance_minor, i64::MAX);
    assert_eq!(rows[0].display, Some(currency("KWD")));
    assert_eq!(rows[0].display_minor, None);
    assert_eq!(rows[0].rate_on, None);
    assert_eq!(rows[0].conversion, ConversionState::TooLarge);
}

fn tricky_date() -> impl Strategy<Value = Date> {
    (
        prop_oneof![
            Just(date(2024, 2, 29)),
            Just(date(2028, 2, 29)),
            Just(date(2026, 2, 28)),
            Just(date(2026, 1, 31)),
            Just(date(2026, 12, 31)),
        ],
        -20_i32..20,
    )
        .prop_map(|(base, offset)| base.checked_add(offset.days()).unwrap())
}

proptest! {
    /// Each day's row comes from the latest snapshot on or before it, whatever
    /// order they were given in, and no day is skipped.
    #[test]
    fn each_day_reads_the_latest_snapshot(
        days in collection::btree_map(tricky_date(), -1000_i64..1000, 0..12),
        from in tricky_date(),
        length in 0_i32..30,
    ) {
        let through = from.checked_add(length.days()).unwrap();
        let mut snapshots: Vec<SnapshotPoint> =
            days.iter().map(|(&on, &minor)| snapshot(on, minor)).collect();
        snapshots.reverse();
        let rows = usd_rows(&snapshots, from, through);

        let first = days.keys().next().map(|&first| first.max(from));
        prop_assert_eq!(rows.first().map(|row| row.on), first.filter(|&on| on <= through));
        for row in &rows {
            let (&balance_on, &minor) = days.range(..=row.on).next_back().unwrap();
            prop_assert_eq!(row.balance_on, balance_on);
            prop_assert_eq!(row.balance_minor, minor);
            prop_assert!(from <= row.on && row.on <= through);
        }
        for pair in rows.windows(2) {
            prop_assert_eq!(pair[1].on, pair[0].on.tomorrow().unwrap());
        }
        prop_assert_eq!(rows.last().is_some(), first.is_some_and(|on| on <= through));
        if let Some(last) = rows.last() {
            prop_assert_eq!(last.on, through);
        }
    }
}
