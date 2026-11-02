//! The periods of a range.

use jiff::ToSpan;
use jiff::civil::{Date, Weekday, date};
use proptest::prelude::{Just, Strategy, prop_assert, prop_assert_eq, prop_oneof, proptest};

use crate::net_worth::{MAX_POINTS, NetWorthError, NetWorthInterval, PeriodEnd, period_ends};

fn end(on: Date, start: Date, partial: bool) -> PeriodEnd {
    PeriodEnd { on, start, partial }
}

#[test]
fn week_ends_on_sunday() {
    let periods = period_ends(
        date(2026, 10, 28),
        date(2026, 11, 3),
        NetWorthInterval::Week,
    )
    .unwrap();
    assert_eq!(
        periods,
        [
            end(date(2026, 11, 1), date(2026, 10, 28), false),
            end(date(2026, 11, 3), date(2026, 11, 2), true),
        ]
    );
}

#[test]
fn a_range_starting_on_sunday_has_a_one_day_week() {
    let sunday = date(2026, 11, 1);
    let periods = period_ends(sunday, date(2026, 11, 8), NetWorthInterval::Week).unwrap();
    assert_eq!(
        periods,
        [
            end(sunday, sunday, false),
            end(date(2026, 11, 8), date(2026, 11, 2), false),
        ]
    );
}

#[test]
fn month_ends_on_its_last_day() {
    let leap = period_ends(date(2028, 2, 1), date(2028, 2, 29), NetWorthInterval::Month).unwrap();
    assert_eq!(leap, [end(date(2028, 2, 29), date(2028, 2, 1), false)]);
    let plain = period_ends(date(2026, 2, 10), date(2026, 3, 1), NetWorthInterval::Month).unwrap();
    assert_eq!(
        plain,
        [
            end(date(2026, 2, 28), date(2026, 2, 10), false),
            end(date(2026, 3, 1), date(2026, 3, 1), true),
        ]
    );
}

#[test]
fn last_period_is_partial() {
    let periods = period_ends(
        date(2028, 1, 31),
        date(2028, 2, 28),
        NetWorthInterval::Month,
    )
    .unwrap();
    assert_eq!(
        periods,
        [
            end(date(2028, 1, 31), date(2028, 1, 31), false),
            end(date(2028, 2, 28), date(2028, 2, 1), true),
        ]
    );
}

#[test]
fn a_day_is_never_partial() {
    let on = date(2028, 2, 29);
    let periods = period_ends(on, on, NetWorthInterval::Day).unwrap();
    assert_eq!(periods, [end(on, on, false)]);
}

#[test]
fn too_many_points_is_refused() {
    let from = date(2026, 1, 1);
    let last_allowed = from.checked_add(999.days()).unwrap();
    let allowed = period_ends(from, last_allowed, NetWorthInterval::Day).unwrap();
    assert_eq!(allowed.len(), 1000);
    assert_eq!(allowed.len(), MAX_POINTS);
    assert_eq!(
        period_ends(
            from,
            last_allowed.tomorrow().unwrap(),
            NetWorthInterval::Day
        ),
        Err(NetWorthError::TooManyPoints)
    );
    assert_eq!(
        period_ends(Date::MIN, Date::MAX, NetWorthInterval::Month),
        Err(NetWorthError::TooManyPoints)
    );
}

#[test]
fn inverted_range_is_refused() {
    assert_eq!(
        period_ends(date(2026, 11, 2), date(2026, 11, 1), NetWorthInterval::Day),
        Err(NetWorthError::RangeInverted)
    );
}

#[test]
fn a_week_past_the_calendar_is_refused_without_a_panic() {
    // 9999-12-31 is a Friday, whose Sunday does not exist.
    assert_eq!(
        period_ends(Date::MAX, Date::MAX, NetWorthInterval::Week),
        Err(NetWorthError::OutOfRange)
    );
}

fn interval() -> impl Strategy<Value = NetWorthInterval> {
    prop_oneof![
        Just(NetWorthInterval::Day),
        Just(NetWorthInterval::Week),
        Just(NetWorthInterval::Month),
    ]
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
        -40_i32..40,
    )
        .prop_map(|(base, offset)| base.checked_add(offset.days()).unwrap())
}

fn is_natural_end(on: Date, interval: NetWorthInterval) -> bool {
    match interval {
        NetWorthInterval::Day => true,
        NetWorthInterval::Week => on.weekday() == Weekday::Sunday,
        NetWorthInterval::Month => on == on.last_of_month(),
    }
}

proptest! {
    /// Periods cover the range with no gap and no overlap, and end where their
    /// kind ends unless they say otherwise.
    #[test]
    fn periods_partition_the_range(
        from in tricky_date(),
        length in 0_i32..300,
        interval in interval(),
    ) {
        let to = from.checked_add(length.days()).unwrap();
        let periods = period_ends(from, to, interval).unwrap();

        prop_assert_eq!(periods[0].start, from);
        prop_assert_eq!(periods[periods.len() - 1].on, to);
        for pair in periods.windows(2) {
            prop_assert_eq!(pair[1].start, pair[0].on.tomorrow().unwrap());
        }
        for period in &periods {
            prop_assert!(period.start <= period.on);
            prop_assert_eq!(period.partial, !is_natural_end(period.on, interval));
            // Only the last may stop short of its period's end.
            prop_assert!(!period.partial || period.on == to);
            // No natural end falls inside a period before its own.
            let mut day = period.start;
            while day < period.on {
                prop_assert!(!is_natural_end(day, interval));
                day = day.tomorrow().unwrap();
            }
        }
    }
}
