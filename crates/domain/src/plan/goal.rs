//! Sinking fund arithmetic (spec §6.7): money is set aside linearly from a
//! goal's start to its due date. Exact, in minor units: intermediates are
//! `i128`, and the one rounding is half-even, at the end.

use jiff::civil::Date;

use crate::plan::days_between;

/// How much of `target_minor` has been set aside at `at`: target × elapsed
/// days / total days, the elapsed days clamped to the period, rounded
/// half-even. A period of no days, which validation refuses, accrues
/// everything on its due date and nothing before.
#[must_use]
pub fn goal_accrued(target_minor: i64, start_on: Date, due_on: Date, at: Date) -> i64 {
    let total = days_between(start_on, due_on);
    if total <= 0 {
        return if at >= due_on { target_minor } else { 0 };
    }
    let elapsed = days_between(start_on, at).clamp(0, total);
    let numerator = i128::from(target_minor) * i128::from(elapsed);
    let total = i128::from(total);
    let floor = numerator.div_euclid(total);
    let twice_rest = numerator.rem_euclid(total) * 2;
    let round_up = twice_rest > total || (twice_rest == total && floor.rem_euclid(2) == 1);
    // elapsed <= total puts the exact value between 0 and the target, both
    // integers, so rounding it stays there and fits: the fallback never runs.
    i64::try_from(floor + i128::from(round_up)).unwrap_or(target_minor)
}

/// What is left of `target_minor` after `spent_minor`, never below zero.
/// `spent_minor` is positive for money spent, already netted of refunds and
/// floored at 0.
#[must_use]
pub fn goal_remaining(target_minor: i64, spent_minor: i64) -> i64 {
    target_minor.saturating_sub(spent_minor).max(0)
}

/// The cash a goal holds back at `at`: what has accrued less what was spent,
/// never below zero and never above what remains of the target.
#[must_use]
pub fn goal_need(
    target_minor: i64,
    spent_minor: i64,
    start_on: Date,
    due_on: Date,
    at: Date,
) -> i64 {
    let unspent = goal_accrued(target_minor, start_on, due_on, at).saturating_sub(spent_minor);
    goal_remaining(target_minor, spent_minor).min(unspent.max(0))
}

#[cfg(test)]
mod tests {
    use jiff::ToSpan;
    use jiff::civil::{Date, date};
    use proptest::prelude::Strategy;

    use super::{goal_accrued, goal_need, goal_remaining};
    use crate::plan::MAX_PLAN_AMOUNT_MINOR;

    const START: Date = date(2027, 1, 1);

    /// `START` plus `n` days.
    fn day(n: i32) -> Date {
        START.checked_add(n.days()).unwrap()
    }

    #[test]
    fn accrual_rounds_half_even() {
        assert_eq!(
            goal_accrued(100, START, day(3), day(1)),
            33,
            "a third rounds down"
        );
        assert_eq!(
            goal_accrued(200, START, day(3), day(1)),
            67,
            "two thirds round up"
        );
        assert_eq!(
            goal_accrued(5, START, day(2), day(1)),
            2,
            "2.5 goes to even 2"
        );
        assert_eq!(
            goal_accrued(7, START, day(2), day(1)),
            4,
            "3.5 goes to even 4"
        );
    }

    #[test]
    fn accrual_clamps_before_start_and_after_due() {
        assert_eq!(
            goal_accrued(100, START, day(10), day(-1)),
            0,
            "before the start"
        );
        assert_eq!(goal_accrued(100, START, day(10), START), 0, "on the start");
        assert_eq!(goal_accrued(100, START, day(10), day(5)), 50, "halfway");
        assert_eq!(
            goal_accrued(100, START, day(10), day(10)),
            100,
            "on the due date"
        );
        assert_eq!(
            goal_accrued(100, START, day(10), day(11)),
            100,
            "after the due date"
        );
    }

    #[test]
    fn accrual_spans_a_leap_day() {
        let start = date(2028, 2, 28);
        let due = date(2028, 3, 1);
        assert_eq!(
            goal_accrued(100, start, due, date(2028, 2, 29)),
            50,
            "February 29 counts"
        );
    }

    #[test]
    fn accrual_with_no_period_does_not_divide_by_zero() {
        assert_eq!(
            goal_accrued(100, START, START, day(-1)),
            0,
            "before a same-day goal"
        );
        assert_eq!(
            goal_accrued(100, START, START, START),
            100,
            "on a same-day goal"
        );
        assert_eq!(
            goal_accrued(100, day(5), START, day(-1)),
            0,
            "before an inverted due date"
        );
        assert_eq!(
            goal_accrued(100, day(5), START, day(1)),
            100,
            "after an inverted due date"
        );
    }

    #[test]
    fn accrual_is_exact_where_a_float_would_not_be() {
        assert_eq!(
            goal_accrued(i64::MAX, START, day(2), day(1)),
            4_611_686_018_427_387_904,
            "half of i64::MAX rounds to even without overflow"
        );
        assert_eq!(
            goal_accrued(i64::MAX, START, day(84_000), day(83_999)),
            9_223_262_234_806_718_012,
            "the product passes i64 and stays exact"
        );
        assert_eq!(
            goal_accrued(MAX_PLAN_AMOUNT_MINOR, START, day(3), day(2)),
            666_666_666_666_667,
            "the largest plan amount"
        );
    }

    #[test]
    fn remaining_is_never_negative() {
        assert_eq!(goal_remaining(100, 0), 100, "nothing spent");
        assert_eq!(goal_remaining(100, 30), 70, "some spent");
        assert_eq!(goal_remaining(100, 100), 0, "all spent");
        assert_eq!(goal_remaining(100, 130), 0, "overspent");
        assert_eq!(
            goal_remaining(i64::MAX, i64::MAX),
            0,
            "extremes do not overflow"
        );
    }

    #[test]
    fn need_is_never_negative_or_above_remaining() {
        let due = day(10);
        assert_eq!(goal_need(100, 0, START, due, day(5)), 50, "accrued so far");
        assert_eq!(
            goal_need(100, 20, START, due, day(5)),
            30,
            "less what was spent"
        );
        assert_eq!(
            goal_need(100, 80, START, due, day(5)),
            0,
            "spent ahead of accrual"
        );
        assert_eq!(
            goal_need(100, 130, START, due, day(10)),
            0,
            "overspent at the due date"
        );
        assert_eq!(
            goal_need(100, 30, START, due, day(20)),
            70,
            "the remaining target at most"
        );
    }

    fn target() -> impl Strategy<Value = i64> {
        1..=MAX_PLAN_AMOUNT_MINOR
    }

    fn spent() -> impl Strategy<Value = i64> {
        0..=MAX_PLAN_AMOUNT_MINOR
    }

    proptest::proptest! {
        #[test]
        fn accrual_is_the_nearest_integer_with_ties_to_even(
            target in target(),
            total in 1..84_000_i32,
            elapsed in 0..84_000_i32,
        ) {
            let elapsed = elapsed.min(total);
            let accrued = goal_accrued(target, START, day(total), day(elapsed));
            let exact = i128::from(target) * i128::from(elapsed);
            let total = i128::from(total);
            let twice_error = (2 * (i128::from(accrued) * total - exact)).abs();
            proptest::prop_assert!(twice_error <= total, "more than half a unit from exact");
            if twice_error == total {
                proptest::prop_assert_eq!(accrued % 2, 0, "a tie goes to the even neighbour");
            }
            proptest::prop_assert!((0..=target).contains(&accrued), "accrual leaves the target");
        }

        #[test]
        fn need_is_monotonic_in_time(
            target in target(),
            spent in spent(),
            total in 1..10_000_i32,
            first in -100..10_100_i32,
            step in 0..1_000_i32,
        ) {
            let due = day(total);
            let earlier = goal_need(target, spent, START, due, day(first));
            let later = goal_need(target, spent, START, due, day(first + step));
            proptest::prop_assert!(earlier <= later, "need fell as time passed");
        }

        #[test]
        fn need_stays_between_zero_and_remaining(
            target in target(),
            spent in spent(),
            total in 1..10_000_i32,
            at in -100..10_100_i32,
        ) {
            let need = goal_need(target, spent, START, day(total), day(at));
            proptest::prop_assert!(need >= 0, "negative need");
            proptest::prop_assert!(need <= goal_remaining(target, spent), "need above remaining");
        }
    }
}
