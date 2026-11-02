//! When a schedule's occurrences fall (spec §6.7, §8.9).

use jiff::Span;
use jiff::civil::Date;

use crate::plan::{IntervalUnit, PlanSchedule, days_between};

const DAYS_PER_WEEK: i64 = 7;
const MONTHS_PER_YEAR: i64 = 12;

impl PlanSchedule {
    /// The occurrences inside `[from, to_exclusive)`, ascending, never after
    /// an end date. A goal has none.
    #[must_use]
    pub fn occurrences(&self, from: Date, to_exclusive: Date) -> Vec<Date> {
        match *self {
            Self::Recurring {
                every,
                unit,
                anchor_on,
                end_on,
            } => {
                let every = i64::from(every);
                if every == 0 {
                    return Vec::new();
                }
                let mut found = Vec::new();
                for k in steps_before(every, unit, anchor_on, from).. {
                    let Some(at) = kth(every, unit, anchor_on, k) else {
                        break;
                    };
                    if at >= to_exclusive || end_on.is_some_and(|end| at > end) {
                        break;
                    }
                    if at >= from {
                        found.push(at);
                    }
                }
                found
            }
            Self::OneOff { on } if (from..to_exclusive).contains(&on) => vec![on],
            Self::OneOff { .. } | Self::Goal { .. } => Vec::new(),
        }
    }
}

/// The `k` to start walking from, so a far-future `from` costs no scan: every
/// occurrence before it lies in an earlier day, week, month or year than
/// `from`, however it clamps, and the one after it in a later one. Walking
/// passes over at most one occurrence.
fn steps_before(every: i64, unit: IntervalUnit, anchor: Date, from: Date) -> i64 {
    let years = i64::from(from.year()) - i64::from(anchor.year());
    let whole_units = match unit {
        IntervalUnit::Days => days_between(anchor, from),
        IntervalUnit::Weeks => days_between(anchor, from) / DAYS_PER_WEEK,
        IntervalUnit::Months => {
            years * MONTHS_PER_YEAR + i64::from(from.month()) - i64::from(anchor.month())
        }
        IntervalUnit::Years => years,
    };
    whole_units.max(0) / every
}

/// The `k`-th occurrence: `anchor + k × every`, clamped to the month's end by
/// jiff. `None` past the end of the calendar.
fn kth(every: i64, unit: IntervalUnit, anchor: Date, k: i64) -> Option<Date> {
    let steps = k.checked_mul(every)?;
    let span = match unit {
        IntervalUnit::Days => Span::new().try_days(steps),
        IntervalUnit::Weeks => Span::new().try_weeks(steps),
        IntervalUnit::Months => Span::new().try_months(steps),
        IntervalUnit::Years => Span::new().try_years(steps),
    };
    anchor.checked_add(span.ok()?).ok()
}

#[cfg(test)]
mod tests {
    use jiff::Span;
    use jiff::civil::{Date, date};
    use proptest::prelude::{Just, Strategy, prop_oneof};

    use super::{kth, steps_before};
    use crate::plan::{IntervalUnit, PlanSchedule};

    fn recurring(every: u16, unit: IntervalUnit, anchor_on: Date) -> PlanSchedule {
        PlanSchedule::Recurring {
            every,
            unit,
            anchor_on,
            end_on: None,
        }
    }

    #[test]
    fn monthly_from_the_31st_clamps_without_drift() {
        let schedule = recurring(1, IntervalUnit::Months, date(2027, 1, 31));
        assert_eq!(
            schedule.occurrences(date(2027, 1, 1), date(2027, 5, 1)),
            [
                date(2027, 1, 31),
                date(2027, 2, 28),
                date(2027, 3, 31),
                date(2027, 4, 30)
            ],
            "each month's last day, March back on the 31st"
        );
    }

    #[test]
    fn every_two_months_clamps_from_the_anchor() {
        let schedule = recurring(2, IntervalUnit::Months, date(2026, 10, 31));
        assert_eq!(
            schedule.occurrences(date(2026, 11, 1), date(2027, 5, 1)),
            [date(2026, 12, 31), date(2027, 2, 28), date(2027, 4, 30)],
            "counted from the anchor"
        );
    }

    #[test]
    fn yearly_from_feb_29() {
        let schedule = recurring(1, IntervalUnit::Years, date(2028, 2, 29));
        assert_eq!(
            schedule.occurrences(date(2029, 1, 1), date(2033, 1, 1)),
            [
                date(2029, 2, 28),
                date(2030, 2, 28),
                date(2031, 2, 28),
                date(2032, 2, 29)
            ],
            "February 28 until the next leap year"
        );
    }

    #[test]
    fn every_two_weeks() {
        let schedule = recurring(2, IntervalUnit::Weeks, date(2026, 11, 2));
        assert_eq!(
            schedule.occurrences(date(2026, 11, 3), date(2026, 12, 1)),
            [date(2026, 11, 16), date(2026, 11, 30)],
            "fourteen days apart, the anchor outside the window"
        );
    }

    #[test]
    fn every_three_days_lands_on_the_window_start() {
        let schedule = recurring(3, IntervalUnit::Days, date(2027, 1, 1));
        assert_eq!(
            schedule.occurrences(date(2027, 1, 7), date(2027, 1, 12)),
            [date(2027, 1, 7), date(2027, 1, 10)],
            "the window start is inclusive"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 1, 8), date(2027, 1, 13)),
            [date(2027, 1, 10)],
            "rounds up to the next occurrence"
        );
    }

    #[test]
    fn a_window_starts_on_a_clamped_occurrence() {
        let schedule = recurring(1, IntervalUnit::Months, date(2027, 1, 31));
        assert_eq!(
            schedule.occurrences(date(2027, 2, 28), date(2027, 3, 1)),
            [date(2027, 2, 28)],
            "the clamped date itself"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 3, 1), date(2027, 4, 1)),
            [date(2027, 3, 31)],
            "the day after it moves to the next month"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 2, 1), date(2027, 2, 28)),
            [] as [Date; 0],
            "a month whose clamped date is outside the window"
        );
    }

    #[test]
    fn end_on_is_inclusive() {
        let ended = |end_on| PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2027, 1, 15),
            end_on: Some(end_on),
        };
        let window = (date(2027, 1, 1), date(2028, 1, 1));
        assert_eq!(
            ended(date(2027, 3, 15)).occurrences(window.0, window.1),
            [date(2027, 1, 15), date(2027, 2, 15), date(2027, 3, 15)],
            "an occurrence on the end date counts"
        );
        assert_eq!(
            ended(date(2027, 3, 14)).occurrences(window.0, window.1),
            [date(2027, 1, 15), date(2027, 2, 15)],
            "none after the end date"
        );
        assert_eq!(
            ended(date(2027, 1, 15)).occurrences(window.0, window.1),
            [date(2027, 1, 15)],
            "an end on the anchor is one occurrence"
        );
        assert_eq!(
            ended(date(2027, 3, 14)).occurrences(date(2027, 6, 1), window.1),
            [] as [Date; 0],
            "a window after the end date is empty"
        );
    }

    #[test]
    fn window_end_is_exclusive() {
        let schedule = recurring(1, IntervalUnit::Months, date(2027, 1, 15));
        assert_eq!(
            schedule.occurrences(date(2027, 1, 15), date(2027, 2, 15)),
            [date(2027, 1, 15)],
            "the start is in, the end is out"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 1, 15), date(2027, 1, 15)),
            [] as [Date; 0],
            "an empty window"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 2, 15), date(2027, 1, 15)),
            [] as [Date; 0],
            "a backwards window"
        );
    }

    #[test]
    fn one_off_only_in_window() {
        let on = date(2027, 3, 15);
        let schedule = PlanSchedule::OneOff { on };
        assert_eq!(
            schedule.occurrences(on, date(2027, 3, 16)),
            [on],
            "on the window start"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 3, 16), date(2028, 1, 1)),
            [] as [Date; 0],
            "before the window"
        );
        assert_eq!(
            schedule.occurrences(date(2027, 1, 1), on),
            [] as [Date; 0],
            "on the excluded window end"
        );
    }

    #[test]
    fn goal_has_no_occurrences() {
        let schedule = PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2027, 6, 1),
            completed: false,
        };
        assert_eq!(
            schedule.occurrences(date(2026, 1, 1), date(2028, 1, 1)),
            [] as [Date; 0],
            "a goal accrues instead"
        );
    }

    #[test]
    fn before_anchor_starts_at_anchor() {
        let schedule = recurring(1, IntervalUnit::Weeks, date(2027, 1, 15));
        assert_eq!(
            schedule.occurrences(date(2026, 1, 1), date(2027, 1, 23)),
            [date(2027, 1, 15), date(2027, 1, 22)],
            "nothing before the anchor"
        );
        assert_eq!(
            schedule.occurrences(date(2026, 1, 1), date(2027, 1, 15)),
            [] as [Date; 0],
            "a window ending on the anchor is empty"
        );
    }

    #[test]
    fn a_far_future_window_needs_no_scan_from_the_anchor() {
        let schedule = recurring(1, IntervalUnit::Days, date(1970, 1, 1));
        assert_eq!(
            schedule.occurrences(date(9000, 1, 1), date(9000, 1, 3)),
            [date(9000, 1, 1), date(9000, 1, 2)],
            "days"
        );
        let schedule = recurring(1, IntervalUnit::Months, date(1970, 1, 31));
        assert_eq!(
            schedule.occurrences(date(9200, 2, 1), date(9200, 3, 1)),
            [date(9200, 2, 29)],
            "months, clamped in a far leap year"
        );
    }

    /// `validate` refuses these, but `occurrences` has to end on any input.
    #[test]
    fn an_unvalidated_every_of_zero_does_not_hang() {
        for unit in [
            IntervalUnit::Days,
            IntervalUnit::Weeks,
            IntervalUnit::Months,
            IntervalUnit::Years,
        ] {
            assert_eq!(
                recurring(0, unit, date(2027, 1, 1))
                    .occurrences(date(2027, 1, 1), date(2028, 1, 1)),
                [] as [Date; 0],
                "an interval of nothing never falls due"
            );
        }
    }

    #[test]
    fn occurrences_stop_at_the_end_of_the_calendar() {
        for (unit, anchor) in [
            (IntervalUnit::Days, date(9999, 12, 29)),
            (IntervalUnit::Weeks, date(9999, 12, 29)),
            (IntervalUnit::Months, date(9999, 12, 29)),
            (IntervalUnit::Years, date(9999, 12, 29)),
        ] {
            assert_eq!(
                recurring(u16::MAX, unit, anchor).occurrences(anchor, Date::MAX),
                [anchor],
                "the next occurrence is past the calendar"
            );
        }
        assert_eq!(
            recurring(u16::MAX, IntervalUnit::Years, date(-9999, 1, 1))
                .occurrences(date(-9999, 1, 1), Date::MAX)
                .len(),
            1,
            "a span too large to represent also stops"
        );
    }

    /// Walks `k` from 0, the rule as the spec states it.
    fn brute_force(
        every: u16,
        unit: IntervalUnit,
        anchor: Date,
        end_on: Option<Date>,
        from: Date,
        to: Date,
    ) -> Vec<Date> {
        let mut found = Vec::new();
        for k in 0_i64.. {
            let steps = k * i64::from(every);
            let span = match unit {
                IntervalUnit::Days => Span::new().days(steps),
                IntervalUnit::Weeks => Span::new().weeks(steps),
                IntervalUnit::Months => Span::new().months(steps),
                IntervalUnit::Years => Span::new().years(steps),
            };
            let at = anchor.checked_add(span).unwrap();
            if at >= to || end_on.is_some_and(|end| at > end) {
                break;
            }
            if at >= from {
                found.push(at);
            }
        }
        found
    }

    fn plan_date() -> impl Strategy<Value = Date> {
        (1970_i16..=2200, 1_i8..=12, 1_i8..=31).prop_map(|(year, month, day)| {
            let last = date(year, month, 1).days_in_month();
            date(year, month, day.min(last))
        })
    }

    fn month_end() -> impl Strategy<Value = Date> {
        (1970_i16..=2200, 1_i8..=12).prop_map(|(year, month)| date(year, month, 1).last_of_month())
    }

    fn unit() -> impl Strategy<Value = IntervalUnit> {
        prop_oneof![
            Just(IntervalUnit::Days),
            Just(IntervalUnit::Weeks),
            Just(IntervalUnit::Months),
            Just(IntervalUnit::Years),
        ]
    }

    proptest::proptest! {
        #[test]
        fn the_fast_forward_lands_next_to_the_first_occurrence(
            every in 1_u16..=999,
            unit in unit(),
            anchor in prop_oneof![plan_date(), month_end()],
            from in plan_date(),
        ) {
            let every = i64::from(every);
            let k = steps_before(every, unit, anchor, from);
            if k > 0 {
                let earlier = kth(every, unit, anchor, k - 1).unwrap();
                proptest::prop_assert!(earlier < from, "the walk starts too late");
            }
            let after = kth(every, unit, anchor, k + 1).unwrap();
            proptest::prop_assert!(after >= from, "the walk starts more than one step early");
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        #[test]
        fn occurrences_match_brute_force(
            every in prop_oneof![1_u16..=3, 1_u16..=999],
            unit in unit(),
            anchor in prop_oneof![plan_date(), month_end()],
            end_on in proptest::option::of(plan_date()),
            from in plan_date(),
            length in prop_oneof![1_i64..=400, 1_i64..=2000],
        ) {
            let to = from.checked_add(Span::new().days(length)).unwrap();
            let schedule = PlanSchedule::Recurring { every, unit, anchor_on: anchor, end_on };
            let found = schedule.occurrences(from, to);
            let expected = brute_force(every, unit, anchor, end_on, from, to);
            proptest::prop_assert_eq!(&found, &expected, "differs from walking every k");
            proptest::prop_assert!(found.windows(2).all(|w| w[0] < w[1]), "not strictly ascending");
            proptest::prop_assert!(found.iter().all(|at| (from..to).contains(at)), "outside the window");
            proptest::prop_assert!(
                found.iter().all(|at| end_on.is_none_or(|end| *at <= end)),
                "after the end date"
            );
        }
    }
}
