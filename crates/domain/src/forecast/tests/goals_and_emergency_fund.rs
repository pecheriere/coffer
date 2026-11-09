//! Sinking funds in the window, and the two kinds of emergency fund target.

use jiff::civil::date;

use super::{cash, eur, every, every_month, fixed_fund, goal, household, inflow, line};
use super::{currency, months_fund};
use super::{has_line, monthly, one_off, run, stored, totals_of, usd, usd_totals};
use crate::forecast::{Caution, ForecastInput, ForecastStatus, ItemInput};
use crate::ids::CategoryId;
use crate::plan::{
    EmergencyFund, EmergencyFundTarget, EmergencyFunds, IntervalUnit, PlanSchedule, PlanTier,
};

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
fn months_target_sums_unreduced_recurring_per_currency() {
    let food = CategoryId::new();
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(10_000_000)), cash("Konto", eur(5_000))],
        horizon_months: 1,
        emergency_funds: months_fund(2),
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 1), home),
            ItemInput {
                tier: Some(PlanTier::Essentials),
                ..super::budget(monthly("Groceries", usd(60_000), date(2026, 1, 20), food))
            },
            monthly("Miete", eur(1_000), date(2026, 1, 28), home),
        ],
        category_spent: vec![crate::forecast::CategorySpent {
            category: food,
            currency: super::currency("USD"),
            spent_minor: 40_000,
        }],
        ..household()
    });
    // The target's window is [Nov 15, Jan 15), whatever the horizon: rent on Dec 1
    // and Jan 1, groceries on Nov 20 and Dec 20 with no reduction.
    assert_eq!(
        totals_of(&forecast, "USD").emergency_fund,
        usd(520_000),
        "400,000 + 120,000"
    );
    assert_eq!(
        totals_of(&forecast, "EUR").emergency_fund,
        eur(2_000),
        "Nov 28 and Dec 28"
    );
    assert_eq!(
        line(&forecast, "Groceries").reduced_by,
        usd(40_000),
        "though the window's own figure is reduced"
    );
}

/// Each currency's months count only its own outflows, over its own number of months.
#[test]
fn months_target_counts_its_own_currency() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(1_000_000)),
            cash("Konto", eur(1_000_000)),
        ],
        emergency_funds: EmergencyFunds::from([
            (
                currency("USD"),
                EmergencyFundTarget::MonthsOfOutflows { months: 3 },
            ),
            (
                currency("EUR"),
                EmergencyFundTarget::MonthsOfOutflows { months: 1 },
            ),
        ]),
        items: vec![
            monthly("Rent", usd(10_000), date(2026, 1, 20), other),
            monthly("Miete", eur(5_000), date(2026, 1, 20), other),
        ],
        ..household()
    });
    assert_eq!(
        totals_of(&forecast, "USD").emergency_fund,
        usd(30_000),
        "Nov 20, Dec 20 and Jan 20 of dollar rent"
    );
    assert_eq!(
        totals_of(&forecast, "EUR").emergency_fund,
        eur(5_000),
        "Nov 20 of euro rent only"
    );
}

/// Must pay and essentials are held for; flexible spending can stop when money is short.
#[test]
fn months_target_counts_must_pay_and_essentials() {
    let home = CategoryId::new();
    let food = CategoryId::new();
    let out = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(3),
        items: vec![
            monthly("Rent", usd(100_000), date(2026, 1, 20), home),
            ItemInput {
                tier: Some(PlanTier::Essentials),
                ..super::budget(monthly("Groceries", usd(40_000), date(2026, 1, 20), food))
            },
            super::budget(monthly("Restaurants", usd(20_000), date(2026, 1, 20), out)),
        ],
        ..household()
    });
    // Nov 20, Dec 20 and Jan 20 of rent and groceries; restaurants are flexible.
    assert_eq!(
        usd_totals(&forecast).emergency_fund,
        usd(420_000),
        "3 × (1,000.00 + 400.00)"
    );
}

#[test]
fn only_flexible_makes_a_zero_months_target() {
    let out = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(3),
        items: vec![super::budget(monthly(
            "Restaurants",
            usd(20_000),
            date(2026, 1, 20),
            out,
        ))],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).emergency_fund,
        usd(0),
        "flexible spending isn't held for"
    );
    assert!(
        forecast
            .cautions
            .contains(&Caution::EmergencyFundMonthsIsZero {
                currency: currency("USD"),
                months: 3,
            }),
        "an emergency fund that came to 0 is not silent"
    );
}

#[test]
fn months_target_ignores_one_offs_goals_and_inflows() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(3),
        items: vec![
            one_off("Repair", usd(1_000), date(2026, 12, 1), other),
            goal("Fund", usd(2_000), date(2026, 1, 1), date(2026, 12, 1), 0),
            inflow(monthly("Pay", usd(4_000), date(2026, 1, 20), other)),
        ],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).emergency_fund,
        usd(0),
        "only recurring outflows"
    );
    assert_eq!(
        forecast.cautions,
        [Caution::EmergencyFundMonthsIsZero {
            currency: currency("USD"),
            months: 3,
        }],
        "an emergency fund that came to 0 is not silent"
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
fn months_target_includes_hypothetical_recurring_items() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(1),
        items: vec![super::hypothetical(
            0,
            monthly("What if", usd(5_000), date(2026, 1, 20), other),
        )],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).emergency_fund,
        usd(5_000),
        "a recurring outflow"
    );
}

#[test]
fn months_target_respects_schedules() {
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(12),
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
        usd_totals(&forecast).emergency_fund,
        usd(3_530),
        "each schedule's own"
    );
}

#[test]
fn a_fixed_fund_is_taken_as_given() {
    let forecast = run(&ForecastInput {
        emergency_funds: fixed_fund(usd(123_456)),
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).emergency_fund,
        usd(123_456),
        "the fixed amount"
    );
    assert_eq!(forecast.surplus, Some(usd(876_544)), "taken off the cash");
    assert_eq!(
        forecast.emergency_funds,
        [EmergencyFund {
            currency: currency("USD"),
            target: EmergencyFundTarget::Fixed {
                minor_units: 123_456
            },
        }],
        "and echoed"
    );
}

#[test]
fn a_months_target_above_zero_does_not_warn() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(1),
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 20), other)],
        ..household()
    });
    assert_eq!(
        forecast.status,
        ForecastStatus::Firm,
        "the emergency fund holds money"
    );
}

#[test]
fn a_months_target_warns_only_for_the_currency_at_zero() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(10)), cash("Konto", eur(10))],
        emergency_funds: months_fund(1),
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 20), other)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::EmergencyFundMonthsIsZero {
            currency: currency("EUR"),
            months: 1,
        }],
        "dollars have an emergency fund and euros do not"
    );
}

#[test]
fn an_explicit_zero_target_does_not_warn() {
    let forecast = run(&household());
    assert_eq!(forecast.cautions, [], "the household said 0");
    assert_eq!(forecast.status, ForecastStatus::Firm, "so it is firm");
}

/// A recurring item whose first occurrence is beyond the window holds nothing, so its
/// currency is not in use, whatever its months target.
#[test]
fn a_foreign_item_outside_the_window_is_not_in_use() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: months_fund(1),
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

/// With no emergency fund there is nothing to dip below, however low the cash goes.
#[test]
fn without_an_emergency_fund_no_dip_is_reported() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        emergency_funds: EmergencyFunds::new(),
        include_income: true,
        items: vec![one_off("Big", usd(5_000_000), date(2026, 12, 1), other)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::EmergencyFundNotSet {
            currency: currency("USD"),
        }],
        "no emergency fund, no dip: only the missing target is said"
    );
    assert_eq!(
        forecast.surplus,
        Some(usd(1_000_000 - 5_000_000)),
        "a number all the same"
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
