//! The report's arithmetic. What the store selects is tested through the router.

use std::collections::BTreeMap;

use jiff::civil::{Date, date};
use proptest::prelude::{ProptestConfig, proptest};

use super::{
    GoalMonth, MonthActual, MonthUncategorized, PlanReport, PlanReportInput, PlanRow, ReportItem,
    plan_vs_actual, report_window,
};
use crate::ids::{CategoryId, PlannedItemId};
use crate::money::{Currency, Money};
use crate::plan::{CalendarMonth, IntervalUnit, PlanSchedule, PlanSpending};
use crate::transactions::CategoryKind;

mod rollover;
mod settled;

fn usd() -> Currency {
    Currency::parse("USD").unwrap()
}

fn eur() -> Currency {
    Currency::parse("EUR").unwrap()
}

fn dollars(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: usd(),
    }
}

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

/// A report of `months` months from 2026-01, on 2026-12-15, so nothing is partial or yet
/// to come, with an average of one month.
fn input(months: u8) -> PlanReportInput {
    PlanReportInput {
        from: month("2026-01"),
        months,
        average_months: 1,
        today: date(2026, 12, 15),
        items: Vec::new(),
        actuals: Vec::new(),
        uncategorized: Vec::new(),
    }
}

fn every_months(every: u16, anchor_on: Date) -> PlanSchedule {
    PlanSchedule::Recurring {
        every,
        unit: IntervalUnit::Months,
        anchor_on,
        end_on: None,
    }
}

fn item(category: CategoryId, minor_units: i64, schedule: PlanSchedule) -> ReportItem {
    ReportItem {
        id: PlannedItemId::new(),
        name: "Item".into(),
        category,
        amount: dollars(minor_units),
        schedule,
        spending: None,
        active: true,
        goal_spent: Vec::new(),
        settled: BTreeMap::new(),
    }
}

fn budget(category: CategoryId, minor_units: i64) -> ReportItem {
    ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(category, minor_units, every_months(1, date(2025, 1, 10)))
    }
}

fn bill(category: CategoryId, minor_units: i64) -> ReportItem {
    ReportItem {
        spending: Some(PlanSpending::Bill),
        ..item(category, minor_units, every_months(1, date(2025, 1, 10)))
    }
}

fn goal(category: CategoryId, minor_units: i64, start_on: Date, due_on: Date) -> ReportItem {
    item(
        category,
        minor_units,
        PlanSchedule::Goal {
            start_on,
            due_on,
            completed: false,
        },
    )
}

fn actual(on: &str, category: CategoryId, kind: CategoryKind, net_minor: i64) -> MonthActual {
    MonthActual {
        month: month(on),
        category,
        kind,
        currency: usd(),
        net_minor,
        left_out_minor: 0,
        possible_duplicate_minor: 0,
    }
}

fn spent(on: &str, category: CategoryId, minor_units: i64) -> MonthActual {
    actual(on, category, CategoryKind::Expense, -minor_units)
}

fn report(input: &PlanReportInput) -> PlanReport {
    plan_vs_actual(input).unwrap()
}

fn only_row(report: &PlanReport) -> PlanRow {
    let [row] = report.rows.as_slice() else {
        panic!("expected exactly one row");
    };
    *row
}

/// The rows of `report` as (month, planned, actual).
fn figures(report: &PlanReport) -> Vec<(String, i64, i64)> {
    report
        .rows
        .iter()
        .map(|row| {
            (
                row.month.to_string(),
                row.planned.minor_units,
                row.actual.minor_units,
            )
        })
        .collect()
}

#[test]
fn occurrences_land_in_their_month() {
    let food = CategoryId::new();
    let weekly = PlanSchedule::Recurring {
        every: 1,
        unit: IntervalUnit::Weeks,
        anchor_on: date(2026, 2, 1),
        end_on: None,
    };
    let one_off = PlanSchedule::OneOff {
        on: date(2026, 2, 28),
    };
    let mut asked = input(1);
    asked.from = month("2026-02");
    asked.items = vec![item(food, 100, weekly), item(food, 7, one_off)];

    // February 2026 has the 1st, 8th, 15th and 22nd; March 1st isn't in it.
    let row = only_row(&report(&asked));

    assert_eq!(row.planned, dollars(407));
}

#[test]
fn bi_monthly_bill_is_not_amortized() {
    let water = CategoryId::new();
    let mut asked = input(3);
    asked.items = vec![bill(water, 600)];
    asked.items[0].schedule = every_months(2, date(2026, 1, 5));

    assert_eq!(
        figures(&report(&asked)),
        [("2026-01".into(), 600, 0), ("2026-03".into(), 600, 0)],
        "the whole amount where it falls, nothing in between"
    );
}

#[test]
fn an_inactive_item_plans_nothing() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![ReportItem {
        active: false,
        ..bill(food, 600)
    }];

    assert_eq!(report(&asked).rows, []);
}

#[test]
fn transfer_categories_are_excluded() {
    let moved = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![MonthActual {
        left_out_minor: 5,
        possible_duplicate_minor: 5,
        ..actual("2026-01", moved, CategoryKind::Transfer, -500)
    }];

    assert_eq!(report(&asked).rows, []);
}

/// 500 spent and 200 refunded reach the report as a net of 300 out.
#[test]
fn refund_nets_against_spending() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![actual("2026-01", food, CategoryKind::Expense, -500 + 200)];

    assert_eq!(only_row(&report(&asked)).actual, dollars(300));
}

#[test]
fn a_month_of_refunds_is_negative_actual() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![actual("2026-01", food, CategoryKind::Expense, 40)];

    assert_eq!(only_row(&report(&asked)).actual, dollars(-40));
}

#[test]
fn income_is_positive() {
    let pay = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![actual("2026-01", pay, CategoryKind::Income, 900)];

    assert_eq!(only_row(&report(&asked)).actual, dollars(900));
}

#[test]
fn difference_is_planned_minus_actual() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![bill(food, 100)];
    asked.actuals = vec![spent("2026-01", food, 130)];

    assert_eq!(only_row(&report(&asked)).difference, dollars(-30));
}

#[test]
fn currencies_are_never_netted() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![bill(food, 100)];
    asked.actuals = vec![MonthActual {
        currency: eur(),
        ..spent("2026-01", food, 70)
    }];

    let rows = report(&asked).rows;

    assert_eq!(rows.len(), 2, "one row per currency");
    assert_eq!(rows[0].currency, eur());
    assert_eq!(rows[0].planned.minor_units, 0);
    assert_eq!(rows[0].actual.minor_units, 70);
    assert_eq!(rows[1].planned, dollars(100));
    assert_eq!(rows[1].actual, dollars(0));
}

#[test]
fn left_out_money_is_reported_and_not_actual() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![MonthActual {
        left_out_minor: 55,
        ..spent("2026-01", food, 10)
    }];

    let row = only_row(&report(&asked));

    assert_eq!(row.actual, dollars(10));
    assert_eq!(row.left_out, dollars(55));
}

#[test]
fn a_row_holding_only_left_out_money_is_kept() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![MonthActual {
        left_out_minor: 55,
        ..spent("2026-01", food, 0)
    }];

    assert_eq!(only_row(&report(&asked)).left_out, dollars(55));
}

#[test]
fn possible_duplicates_are_reported_within_actual() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![MonthActual {
        possible_duplicate_minor: 10,
        ..spent("2026-01", food, 30)
    }];

    let row = only_row(&report(&asked));

    assert_eq!(row.actual, dollars(30));
    assert_eq!(row.possible_duplicate, dollars(10));
}

#[test]
fn a_row_of_zeros_is_left_out() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![spent("2026-01", food, 0)];

    assert_eq!(report(&asked).rows, []);
}

#[test]
fn goal_split_counts_against_the_goal_not_the_category() {
    let home = CategoryId::new();
    let mut roof = goal(home, 1_000, date(2020, 1, 1), date(2021, 1, 1));
    roof.goal_spent = vec![GoalMonth {
        month: month("2026-01"),
        spent_minor: 300,
        left_out_minor: 20,
    }];
    let id = roof.id;
    let mut asked = input(1);
    asked.items = vec![roof];

    let answer = report(&asked);

    assert_eq!(answer.rows, [], "goals are not in rows");
    assert_eq!(answer.goals.len(), 1);
    assert_eq!(answer.goals[0].goal, id);
    assert_eq!(answer.goals[0].spent_against, dollars(300));
    assert_eq!(answer.goals[0].left_out, dollars(20));
}

#[test]
fn goal_accrues_across_the_month_end_on_the_next_first_day() {
    let home = CategoryId::new();
    let mut asked = input(1);
    // 31 of the 62 days from January 1st to March 4th fall in January.
    asked.items = vec![goal(home, 620, date(2026, 1, 1), date(2026, 3, 4))];

    let answer = report(&asked);

    assert_eq!(answer.goals[0].accrued, dollars(310));
    assert_eq!(answer.goals[0].currency, usd());
}

#[test]
fn an_inactive_goal_accrues_nothing_but_keeps_its_spending() {
    let home = CategoryId::new();
    let mut roof = goal(home, 620, date(2026, 1, 1), date(2026, 3, 4));
    roof.active = false;
    roof.goal_spent = vec![GoalMonth {
        month: month("2026-01"),
        spent_minor: 300,
        left_out_minor: 0,
    }];
    let mut asked = input(1);
    asked.items = vec![roof];

    let answer = report(&asked);

    assert_eq!(answer.goals[0].accrued, dollars(0));
    assert_eq!(answer.goals[0].spent_against, dollars(300));
}

#[test]
fn a_goal_month_with_nothing_to_say_is_left_out() {
    let home = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![goal(home, 620, date(2027, 1, 1), date(2027, 3, 4))];

    assert_eq!(report(&asked).goals, []);
}

/// An average over 2 months of (5, 0), (7, 0), and over 3 of (100, 0, 100).
#[test]
fn rolling_average_rounds_half_even_and_counts_zero_months() {
    let cases: [(u8, &[i64], i64); 3] = [(2, &[5, 0], 2), (2, &[7, 0], 4), (3, &[100, 0, 100], 67)];
    for (window, spending, expected) in cases {
        let food = CategoryId::new();
        let first = 3 - i32::from(window);
        let mut asked = input(1);
        asked.from = month("2026-03");
        asked.average_months = window;
        asked.actuals = (0_i32..)
            .zip(spending)
            .map(|(offset, &minor_units)| {
                spent(&format!("2026-0{}", first + 1 + offset), food, minor_units)
            })
            .collect();

        assert_eq!(
            report(&asked).rows[0].rolling_average,
            dollars(expected),
            "the window's mean, half-even"
        );
    }
}

#[test]
fn only_months_in_the_window_count_toward_the_average() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.from = month("2026-03");
    asked.average_months = 2;
    asked.actuals = vec![
        spent("2026-01", food, 1_000),
        spent("2026-02", food, 10),
        spent("2026-03", food, 20),
    ];

    assert_eq!(report(&asked).rows[0].rolling_average, dollars(15));
}

#[test]
fn lookback_months_get_no_row_of_their_own() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.from = month("2026-03");
    asked.average_months = 2;
    asked.actuals = vec![spent("2026-02", food, 10)];

    let row = only_row(&report(&asked));

    assert_eq!(row.month, month("2026-03"));
    assert_eq!(row.actual, dollars(0));
    assert_eq!(row.rolling_average, dollars(5));
}

#[test]
fn only_reported_months_are_uncategorized() {
    let mut asked = input(1);
    asked.from = month("2026-03");
    asked.uncategorized = ["2026-02", "2026-03"]
        .map(|on| MonthUncategorized {
            month: month(on),
            currency: usd(),
            count: 2,
            total_minor: 90,
        })
        .to_vec();

    let answer = report(&asked);

    assert_eq!(answer.uncategorized.len(), 1);
    assert_eq!(answer.uncategorized[0].month, month("2026-03"));
    assert_eq!(answer.uncategorized[0].count, 2);
    assert_eq!(answer.uncategorized[0].total, dollars(90));
}

#[test]
fn current_month_is_partial() {
    let mut asked = input(2);
    asked.from = month("2026-11");
    assert_eq!(report(&asked).partial_months, [month("2026-12")]);
}

#[test]
fn a_report_without_today_has_no_partial_month() {
    assert_eq!(report(&input(2)).partial_months, []);
}

#[test]
fn window_reaches_back_for_the_longest_average_and_past_the_last_month() {
    assert_eq!(
        report_window(month("2026-03"), 2, 3),
        Ok((date(2026, 1, 1), date(2026, 5, 1)))
    );
    assert_eq!(
        report_window(month("2026-03"), 1, 1),
        Ok((date(2026, 3, 1), date(2026, 4, 1)))
    );
}

#[test]
fn a_total_too_large_refuses_the_report() {
    let food = CategoryId::new();
    let mut asked = input(2);
    asked.actuals = vec![MonthActual {
        net_minor: i64::MIN,
        ..spent("2026-01", food, 0)
    }];

    assert!(plan_vs_actual(&asked).is_err(), "never a wrapped number");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the rounding each month, the months add up to the whole target.
    #[test]
    fn goal_accrual_by_month_telescopes_to_the_total(
        target in 1_i64..=1_000_000_000_000_000,
        days in 1_i32..=1_000,
        offset in 0_i32..=27,
    ) {
        let start = date(2026, 1, 1 + i8::try_from(offset).unwrap());
        let due = start.checked_add(jiff::Span::new().days(days)).unwrap();
        let home = CategoryId::new();
        let mut asked = input(36);
        asked.items = vec![goal(home, target, start, due)];

        let total: i128 = report(&asked)
            .goals
            .iter()
            .map(|row| i128::from(row.accrued.minor_units))
            .sum();

        assert_eq!(total, i128::from(target));
    }
}
