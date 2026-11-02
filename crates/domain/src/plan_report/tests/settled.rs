//! R9a: a settled occurrence stays in `planned`, and the report says it is settled.

use jiff::civil::date;

use super::{bill, budget, dollars, input, month, only_row, report, spent};
use crate::ids::CategoryId;
use crate::plan::{IntervalUnit, PlanSchedule, SettlementKind};

#[test]
fn a_settled_month_stays_planned_and_says_how_much_is_settled() {
    let food = CategoryId::new();
    let mut asked = input(1);
    let mut item = bill(food, 600);
    item.settled.insert(month("2026-01"), SettlementKind::Paid);
    let id = item.id;
    asked.items = vec![item];

    let answer = report(&asked);

    let row = only_row(&answer);
    assert_eq!(row.planned, dollars(600), "what was planned");
    assert_eq!(row.settled, dollars(600), "and how much of it is settled");
    let [settled] = answer.settled.as_slice() else {
        panic!("one settled row");
    };
    assert_eq!(settled.item, id, "which item");
    assert_eq!(settled.kind, SettlementKind::Paid, "which kind");
    assert_eq!(settled.amount, dollars(600), "its whole amount");
}

/// A settlement names a month, so it says nothing of another one, and one for a month with
/// no occurrence has nothing to show.
#[test]
fn only_the_settled_months_occurrences_show() {
    let food = CategoryId::new();
    let mut asked = input(2);
    let mut item = bill(food, 600);
    item.schedule = PlanSchedule::Recurring {
        every: 2,
        unit: IntervalUnit::Months,
        anchor_on: date(2026, 1, 10),
        end_on: None,
    };
    item.settled
        .insert(month("2026-02"), SettlementKind::Skipped);
    asked.items = vec![item];

    let answer = report(&asked);

    assert_eq!(
        only_row(&answer).settled,
        dollars(0),
        "January isn't settled"
    );
    assert_eq!(answer.settled, [], "February holds no occurrence");
}

/// Two bills in a category, one settled: the row's `settled` is that one's part.
#[test]
fn a_row_says_only_the_settled_part() {
    let food = CategoryId::new();
    let mut asked = input(1);
    let mut settled = bill(food, 600);
    settled
        .settled
        .insert(month("2026-01"), SettlementKind::Paid);
    asked.items = vec![settled, bill(food, 100)];

    let answer = report(&asked);

    let row = only_row(&answer);
    assert_eq!(row.planned, dollars(700), "both planned");
    assert_eq!(row.settled, dollars(600), "one settled");
}

/// A rollover is for budgets, so it reads as it does without the settlement: the 600 spent
/// pays the 100 bill first, so 500 of it goes on the budget, settled bill or not.
#[test]
fn a_rollover_does_not_read_a_settlement() {
    let food = CategoryId::new();
    let mut asked = input(1);
    asked.actuals = vec![spent("2026-01", food, 600)];
    let mut settled = bill(food, 100);
    asked.items = vec![settled.clone(), budget(food, 800)];
    let plain = report(&asked).rollovers;

    settled
        .settled
        .insert(month("2026-01"), SettlementKind::Paid);
    asked.items = vec![settled, budget(food, 800)];

    let answer = report(&asked);
    assert_eq!(answer.rollovers, plain, "as it was");
    let [line] = answer.rollovers.as_slice() else {
        panic!("one rollover");
    };
    assert_eq!(line.spent, dollars(500), "the bill was paid first");
}
