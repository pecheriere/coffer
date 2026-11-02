//! What each category with a budget has left (R6, R9).

use jiff::civil::date;

use super::{actual, bill, budget, dollars, every_months, input, item, report, spent};
use super::{eur, month, usd};
use crate::ids::CategoryId;
use crate::money::Money;
use crate::plan::{IntervalUnit, PlanSchedule, PlanSpending};
use crate::plan_report::{MonthActual, ReportItem};
use crate::transactions::CategoryKind;

/// A budget of 100 a month: 40 left in January, 30 overspent in February.
#[test]
fn rollover_adds_up_what_was_left_and_overspent() {
    let food = CategoryId::new();
    let mut asked = input(2);
    asked.items = vec![budget(food, 100)];
    asked.actuals = vec![spent("2026-01", food, 60), spent("2026-02", food, 130)];

    let answer = report(&asked);

    let [row] = answer.rollovers.as_slice() else {
        panic!("expected one rollover");
    };
    assert_eq!(row.category, food);
    assert_eq!(row.currency, usd());
    assert_eq!(row.budgeted, dollars(200));
    assert_eq!(row.spent, dollars(190));
    assert_eq!(row.rollover, dollars(10));
}

#[test]
fn overspending_is_a_negative_rollover() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![budget(food, 100)];
    asked.actuals = vec![spent("2026-01", food, 130)];

    assert_eq!(report(&asked).rollovers[0].rollover, dollars(-30));
}

#[test]
fn a_bill_has_no_rollover() {
    let rent = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![bill(rent, 100)];
    asked.actuals = vec![spent("2026-01", rent, 60)];

    assert_eq!(report(&asked).rollovers, []);
}

/// R9: one pool per category, and a paid bill never eats a budget.
#[test]
fn a_bill_in_the_category_takes_its_share_of_the_spending_first() {
    let house = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![budget(house, 100), bill(house, 500)];
    asked.actuals = vec![spent("2026-01", house, 560)];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.budgeted, dollars(100));
    assert_eq!(row.spent, dollars(60));
    assert_eq!(row.rollover, dollars(40));
}

#[test]
fn an_unpaid_bill_doesnt_add_to_the_budget() {
    let house = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![budget(house, 100), bill(house, 500)];
    asked.actuals = vec![spent("2026-01", house, 60)];

    assert_eq!(report(&asked).rollovers[0].rollover, dollars(100));
}

#[test]
fn a_month_of_refunds_adds_to_the_rollover() {
    let food = CategoryId::new();
    let mut asked = input(2);
    asked.items = vec![budget(food, 100)];
    asked.actuals = vec![
        spent("2026-01", food, 130),
        actual("2026-02", food, CategoryKind::Expense, 20),
    ];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.spent, dollars(110));
    assert_eq!(row.rollover, dollars(90));
}

#[test]
fn a_month_yet_to_come_has_no_rollover() {
    let food = CategoryId::new();
    let mut asked = input(3);
    asked.from = month("2026-11");
    asked.items = vec![budget(food, 100)];
    asked.actuals = vec![spent("2026-11", food, 60)];

    let row = report(&asked).rollovers[0];

    // November and December, today's month; not January.
    assert_eq!(row.budgeted, dollars(200));
    assert_eq!(row.rollover, dollars(140));
}

/// R9: a bill counts as paid only once its date has passed, so today's month keeps what
/// was spent on the budget until then.
#[test]
fn a_bill_dated_from_today_on_takes_no_share_of_the_spending() {
    let house = CategoryId::new();
    let mut asked = input(1);
    asked.from = month("2026-12");
    let mut due_today = bill(house, 500);
    due_today.schedule = every_months(1, date(2025, 1, 15));
    let mut due_before = bill(house, 7);
    due_before.schedule = every_months(1, date(2025, 1, 14));
    asked.items = vec![budget(house, 100), due_today, due_before];
    asked.actuals = vec![spent("2026-12", house, 67)];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.spent, dollars(60));
}

/// Spending from before a budget's first occurrence isn't charged to it.
#[test]
fn spending_before_a_budget_starts_is_not_charged_to_it() {
    let food = CategoryId::new();
    let mut asked = input(4);
    asked.from = month("2026-08");
    asked.today = date(2026, 11, 15);
    asked.items = vec![ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(food, 100, every_months(1, date(2026, 10, 10)))
    }];
    asked.actuals = vec![
        spent("2026-08", food, 500),
        spent("2026-09", food, 500),
        spent("2026-10", food, 50),
        spent("2026-11", food, 30),
    ];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.budgeted, dollars(200));
    assert_eq!(row.spent, dollars(80));
    assert_eq!(row.rollover, dollars(120));
}

/// Nor is spending from after the month of its end date.
#[test]
fn spending_after_a_budget_ends_is_not_charged_to_it() {
    let food = CategoryId::new();
    let mut asked = input(4);
    asked.from = month("2026-08");
    asked.today = date(2026, 11, 15);
    asked.items = vec![ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(
            food,
            100,
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on: date(2026, 8, 10),
                end_on: Some(date(2026, 9, 30)),
            },
        )
    }];
    asked.actuals = vec![
        spent("2026-08", food, 50),
        spent("2026-09", food, 60),
        spent("2026-10", food, 500),
        spent("2026-11", food, 500),
    ];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.budgeted, dollars(200));
    assert_eq!(row.spent, dollars(110));
    assert_eq!(row.rollover, dollars(90));
}

/// The month of the end date counts whole, even after the date itself.
#[test]
fn the_month_a_budget_ends_counts_whole() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.from = month("2026-09");
    asked.items = vec![ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(
            food,
            100,
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on: date(2026, 8, 10),
                end_on: Some(date(2026, 9, 12)),
            },
        )
    }];
    asked.actuals = vec![spent("2026-09", food, 60)];

    assert_eq!(report(&asked).rollovers[0].spent, dollars(60));
}

/// Ending on the 1st still takes in that month.
#[test]
fn a_budget_ending_on_the_first_runs_through_that_month() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.from = month("2026-09");
    asked.items = vec![ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(
            food,
            100,
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on: date(2026, 8, 1),
                end_on: Some(date(2026, 9, 1)),
            },
        )
    }];
    asked.actuals = vec![spent("2026-09", food, 60)];

    assert_eq!(report(&asked).rollovers[0].spent, dollars(60));
}

/// A budget every other month is running between its occurrences.
#[test]
fn a_bi_monthly_budgets_off_month_still_counts() {
    let food = CategoryId::new();
    let mut asked = input(2);
    asked.items = vec![ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(food, 100, every_months(2, date(2026, 1, 10)))
    }];
    asked.actuals = vec![spent("2026-01", food, 10), spent("2026-02", food, 20)];

    let row = report(&asked).rollovers[0];

    assert_eq!(row.budgeted, dollars(100));
    assert_eq!(row.spent, dollars(30));
}

/// One budget running is enough, and a month between two budgets counts for neither.
#[test]
fn a_month_counts_while_any_budget_in_the_category_runs() {
    let food = CategoryId::new();
    let running = |anchor_on, end_on| ReportItem {
        spending: Some(PlanSpending::Budget),
        ..item(
            food,
            100,
            PlanSchedule::Recurring {
                every: 1,
                unit: IntervalUnit::Months,
                anchor_on,
                end_on: Some(end_on),
            },
        )
    };
    let mut asked = input(3);
    asked.items = vec![
        running(date(2026, 1, 10), date(2026, 1, 31)),
        running(date(2026, 3, 10), date(2026, 3, 31)),
    ];
    asked.actuals = vec![
        spent("2026-01", food, 1),
        spent("2026-02", food, 20),
        spent("2026-03", food, 300),
    ];

    assert_eq!(report(&asked).rollovers[0].spent, dollars(301));
}

/// A budget in one currency says nothing of the other's spending in the category.
#[test]
fn a_budget_runs_only_in_its_own_currency() {
    let food = CategoryId::new();
    let euro_budget = ReportItem {
        amount: Money {
            minor_units: 100,
            currency: eur(),
        },
        ..budget(food, 100)
    };
    let mut asked = input(1);
    asked.items = vec![budget(food, 100), euro_budget];
    asked.actuals = vec![spent("2026-01", food, 30)];

    let rows = report(&asked).rollovers;

    assert_eq!(rows.len(), 2, "one line per currency");
    assert_eq!(rows[0].currency, eur());
    assert_eq!(rows[0].spent.minor_units, 0);
    assert_eq!(rows[1].spent, dollars(30));
}

/// An inactive budget has stopped, so it keeps no month running.
#[test]
fn an_inactive_budget_keeps_no_month_running() {
    let food = CategoryId::new();
    let mut asked = input(2);
    let mut live = item(food, 100, every_months(1, date(2026, 2, 10)));
    live.spending = Some(PlanSpending::Budget);
    let stopped = ReportItem {
        active: false,
        ..budget(food, 100)
    };
    asked.items = vec![live, stopped];
    asked.actuals = vec![spent("2026-01", food, 40), spent("2026-02", food, 5)];

    assert_eq!(report(&asked).rollovers[0].spent, dollars(5));
}

/// A bill in the category doesn't keep the month running for a budget.
#[test]
fn a_bill_keeps_no_month_running() {
    let house = CategoryId::new();
    let mut asked = input(2);
    let mut late = item(house, 100, every_months(1, date(2026, 2, 10)));
    late.spending = Some(PlanSpending::Budget);
    asked.items = vec![late, bill(house, 7)];
    asked.actuals = vec![spent("2026-01", house, 40), spent("2026-02", house, 7)];

    assert_eq!(report(&asked).rollovers[0].spent, dollars(0));
}

#[test]
fn a_rollover_is_per_currency() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![budget(food, 100)];
    asked.actuals = vec![MonthActual {
        currency: eur(),
        ..spent("2026-01", food, 70)
    }];

    let answer = report(&asked);

    assert_eq!(answer.rollovers.len(), 1, "no budget in the other currency");
    assert_eq!(answer.rollovers[0].rollover, dollars(100));
}

#[test]
fn an_inactive_budget_has_no_rollover() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.items = vec![ReportItem {
        active: false,
        ..budget(food, 100)
    }];

    assert_eq!(report(&asked).rollovers, []);
}
