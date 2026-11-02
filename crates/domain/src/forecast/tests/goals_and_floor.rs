//! Sinking funds in the window, and the two kinds of safety floor.

use jiff::civil::date;

use super::{cash, eur, every, every_month, fixed_floor, goal, household, inflow, line};
use super::{has_line, monthly, one_off, run, stored, totals_of, usd, usd_totals};
use crate::forecast::{Caution, ForecastInput, ForecastStatus, ItemInput};
use crate::ids::CategoryId;
use crate::plan::{IntervalUnit, PlanSchedule, SafetyFloor};

#[test]
fn completed_goal_not_counted() {
    let fund = goal("Done", usd(50_000), date(2026, 1, 1), date(2026, 12, 1), 0);
    let completed = ItemInput {
        schedule: PlanSchedule::Goal {
            start_on: date(2026, 1, 1),
            due_on: date(2026, 12, 1),
            completed: true,
        },
        ..fund
    };
    let forecast = run(&ForecastInput {
        items: vec![completed],
        ..household()
    });
    assert!(!has_line(&forecast, "Done"), "no line for it");
    assert_eq!(usd_totals(&forecast).goals, usd(0), "no need");
    assert_eq!(forecast.status, ForecastStatus::Firm, "and no caution");
}

#[test]
fn a_completed_overdue_goal_does_not_warn() {
    let forecast = run(&ForecastInput {
        items: vec![ItemInput {
            schedule: PlanSchedule::Goal {
                start_on: date(2025, 1, 1),
                due_on: date(2026, 1, 1),
                completed: true,
            },
            ..stored(
                "Done",
                usd(1),
                PlanSchedule::OneOff {
                    on: date(2026, 1, 1),
                },
            )
        }],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "finished is finished");
}

#[test]
fn goal_due_on_horizon_end_is_full_at_end() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "Edge",
            usd(90_000),
            date(2026, 2, 15),
            date(2027, 2, 15),
            10_000,
        )],
        ..household()
    });
    let edge = line(&forecast, "Edge");
    assert_eq!(edge.counted, usd(80_000), "the whole remaining target");
    assert!(
        !edge.goal.unwrap().due_in_horizon,
        "the end of the window is not in it"
    );
    assert_eq!(forecast.cautions, [], "not overdue");
}

#[test]
fn goal_due_on_as_of_counts_full_remaining_without_warning() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "Today",
            usd(90_000),
            date(2026, 2, 15),
            date(2026, 11, 15),
            10_000,
        )],
        ..household()
    });
    let today = line(&forecast, "Today");
    assert_eq!(today.counted, usd(80_000), "the whole remaining target");
    assert!(today.goal.unwrap().due_in_horizon, "as_of is in the window");
    assert_eq!(forecast.cautions, [], "due today is not overdue");
}

#[test]
fn goal_due_later_counts_what_has_accrued_at_the_end() {
    // From Dec 12, 2025 the due date is 800 days on, and the window's end, Feb 15,
    // 2027, is 430 days on: 800,000 × 430 / 800 = 430,000.
    let start = date(2025, 12, 12);
    let forecast = run(&ForecastInput {
        items: vec![goal("Long", usd(800_000), start, date(2028, 2, 20), 30_000)],
        ..household()
    });
    assert_eq!(
        line(&forecast, "Long").counted,
        usd(400_000),
        "430,000 accrued less 30,000 spent"
    );
}

#[test]
fn goal_not_started_counts_nothing() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "Future",
            usd(90_000),
            date(2027, 2, 15),
            date(2028, 2, 15),
            0,
        )],
        ..household()
    });
    assert!(
        !has_line(&forecast, "Future"),
        "it starts on the window's end, with nothing set aside"
    );
    assert_eq!(usd_totals(&forecast).goals, usd(0), "no need");
}

#[test]
fn overspent_goal_counts_zero() {
    let fund = goal(
        "Overspent",
        usd(90_000),
        date(2026, 1, 1),
        date(2026, 12, 1),
        120_000,
    );
    let forecast = run(&ForecastInput {
        items: vec![fund],
        ..household()
    });
    assert!(!has_line(&forecast, "Overspent"), "gross is zero");
    assert_eq!(usd_totals(&forecast).goals, usd(0), "no negative need");
    assert_eq!(
        forecast.surplus,
        Some(usd(1_000_000)),
        "no extra cash either"
    );
}

#[test]
fn foreign_goal_with_nothing_to_count_is_not_mixed() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "Euro fund",
            eur(90_000),
            date(2026, 1, 1),
            date(2026, 12, 1),
            90_000,
        )],
        ..household()
    });
    assert_eq!(
        forecast.status,
        ForecastStatus::Firm,
        "gross zero is unused"
    );
}

#[test]
fn goal_spending_is_never_read_as_negative() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "Refunded",
            usd(90_000),
            date(2026, 1, 1),
            date(2026, 12, 1),
            -5_000,
        )],
        ..household()
    });
    let refunded = line(&forecast, "Refunded");
    assert_eq!(refunded.counted, usd(90_000), "never above the target");
    assert_eq!(
        refunded.goal.unwrap().spent,
        usd(0),
        "shown as nothing spent"
    );
}

#[test]
fn months_floor_sums_unreduced_recurring_per_currency() {
    let food = CategoryId::new();
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(10_000_000)), cash("Konto", eur(5_000))],
        horizon_months: 1,
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 2 }),
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 1), home),
            super::budget(monthly("Groceries", usd(60_000), date(2026, 1, 20), food)),
            monthly("Miete", eur(1_000), date(2026, 1, 28), home),
        ],
        category_spent: vec![crate::forecast::CategorySpent {
            category: food,
            currency: super::currency("USD"),
            spent_minor: 40_000,
        }],
        ..household()
    });
    // The floor's window is [Nov 15, Jan 15), whatever the horizon: rent on Dec 1
    // and Jan 1, groceries on Nov 20 and Dec 20 with no reduction.
    assert_eq!(
        totals_of(&forecast, "USD").floor,
        usd(520_000),
        "400,000 + 120,000"
    );
    assert_eq!(
        totals_of(&forecast, "EUR").floor,
        eur(2_000),
        "Nov 28 and Dec 28"
    );
    assert_eq!(
        line(&forecast, "Groceries").reduced_by,
        usd(40_000),
        "though the window's own figure is reduced"
    );
}

#[test]
fn months_floor_ignores_one_offs_goals_and_inflows() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 3 }),
        items: vec![
            one_off("Repair", usd(1_000), date(2026, 12, 1), other),
            goal("Fund", usd(2_000), date(2026, 1, 1), date(2026, 12, 1), 0),
            inflow(monthly("Pay", usd(4_000), date(2026, 1, 20), other)),
        ],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).floor,
        usd(0),
        "only recurring outflows"
    );
    assert_eq!(
        forecast.cautions,
        [Caution::MonthsFloorIsZero {
            currency: super::currency("USD"),
            months: 3,
        }],
        "a floor that came to 0 is not silent"
    );
    assert_eq!(
        forecast.status,
        ForecastStatus::Provisional,
        "the number stays"
    );
    assert_eq!(
        forecast.surplus,
        Some(usd(1_000_000 - 1_000 - 2_000)),
        "as computed"
    );
}

#[test]
fn months_floor_includes_hypothetical_recurring_items() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 1 }),
        items: vec![super::hypothetical(
            0,
            monthly("What if", usd(5_000), date(2026, 1, 20), other),
        )],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).floor,
        usd(5_000),
        "a recurring outflow"
    );
}

#[test]
fn months_floor_respects_schedules() {
    let forecast = run(&ForecastInput {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 12 }),
        items: vec![
            stored(
                "Weekly",
                usd(10),
                every(1, IntervalUnit::Weeks, date(2026, 11, 15)),
            ),
            stored(
                "Ends",
                usd(1_000),
                PlanSchedule::Recurring {
                    every: 1,
                    unit: IntervalUnit::Months,
                    anchor_on: date(2026, 11, 20),
                    end_on: Some(date(2027, 1, 20)),
                },
            ),
            stored("Late start", usd(100_000), every_month(date(2027, 11, 15))),
        ],
        ..household()
    });
    // [Nov 15, 2026, Nov 15, 2027) is 365 days, 53 weeks counting the first day
    // (Nov 15 itself to Nov 14, 2027): 530. Ends: Nov, Dec, Jan: 3,000. Late start
    // begins on the excluded end.
    assert_eq!(
        usd_totals(&forecast).floor,
        usd(3_530),
        "each schedule's own"
    );
}

#[test]
fn a_fixed_floor_is_taken_as_given() {
    let forecast = run(&ForecastInput {
        floor: Some(fixed_floor(usd(123_456))),
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).floor,
        usd(123_456),
        "the fixed amount"
    );
    assert_eq!(forecast.surplus, Some(usd(876_544)), "taken off the cash");
    assert_eq!(
        forecast.floor,
        Some(fixed_floor(usd(123_456))),
        "and echoed"
    );
}

#[test]
fn a_months_floor_above_zero_does_not_warn() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 1 }),
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 20), other)],
        ..household()
    });
    assert_eq!(
        forecast.status,
        ForecastStatus::Firm,
        "the floor holds money"
    );
}

#[test]
fn a_months_floor_warns_only_for_the_currency_at_zero() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(10)), cash("Konto", eur(10))],
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 1 }),
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 20), other)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::MonthsFloorIsZero {
            currency: super::currency("EUR"),
            months: 1,
        }],
        "dollars have a floor and euros do not"
    );
}

#[test]
fn an_explicit_zero_floor_does_not_warn() {
    let forecast = run(&household());
    assert_eq!(forecast.cautions, [], "the household said 0");
    assert_eq!(forecast.status, ForecastStatus::Firm, "so it is firm");
}

/// A recurring item whose first occurrence is beyond the floor's months holds
/// nothing, so its currency is not in use.
#[test]
fn a_foreign_item_outside_the_floor_months_is_not_in_use() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 1 }),
        items: vec![
            monthly("Rent", usd(1_000), date(2026, 1, 20), other),
            monthly("Miete", eur(1_000), date(2030, 1, 20), other),
        ],
        ..household()
    });
    assert_eq!(forecast.blockers, [], "not mixed");
    assert_eq!(forecast.totals.len(), 1, "no row for euros");
    assert_eq!(forecast.status, ForecastStatus::Firm, "firm");
}

/// With no floor there is nothing to dip below, however low the cash goes.
#[test]
fn without_a_floor_no_dip_is_reported() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        floor: None,
        include_income: true,
        items: vec![one_off("Big", usd(5_000_000), date(2026, 12, 1), other)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "no floor, no dip");
    assert_eq!(
        forecast.status,
        ForecastStatus::Unavailable,
        "and no number"
    );
}

/// A bucket's need is measured at its end, which for November is December 1, the
/// due date: the goal is already whole there, so December adds nothing.
#[test]
fn a_goal_due_on_the_first_fills_the_month_before() {
    let forecast = run(&ForecastInput {
        items: vec![goal(
            "First",
            usd(90_000),
            date(2026, 1, 1),
            date(2026, 12, 1),
            0,
        )],
        ..household()
    });
    let accruals: Vec<_> = forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().goal_accrual)
        .collect();
    assert_eq!(
        accruals,
        [usd(90_000), usd(0), usd(0), usd(0)],
        "all by the end of November"
    );
    assert!(
        line(&forecast, "First").goal.unwrap().due_in_horizon,
        "though it is due in the window"
    );
}
