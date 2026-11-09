//! Each month reads the version of an item in force then (spec §10.3).

use jiff::civil::date;

use super::{every_months, figures, input, item, month, report, versions};
use crate::ids::CategoryId;
use crate::plan::PlanSpending;
use crate::plan_report::ReportItem;

fn rent_raised_in_october(category: CategoryId) -> ReportItem {
    ReportItem {
        versions: versions(&[
            (None, category, 1000, None),
            (Some("2026-10"), category, 1100, None),
        ]),
        ..item(category, 1000, every_months(1, date(2025, 1, 10)))
    }
}

#[test]
fn plan_vs_actual_reads_each_months_version() {
    let rent = CategoryId::new();
    let mut asked = input(2);
    asked.from = month("2026-09");
    asked.items = vec![rent_raised_in_october(rent)];

    assert_eq!(
        figures(&report(&asked)),
        [("2026-09".into(), 1000, 0), ("2026-10".into(), 1100, 0)]
    );
}

#[test]
fn a_category_change_moves_the_planned_amount_between_categories() {
    let groceries = CategoryId::new();
    let household = CategoryId::new();
    let mut asked = input(2);
    asked.from = month("2026-09");
    asked.items = vec![ReportItem {
        versions: versions(&[
            (None, groceries, 300, Some(PlanSpending::Budget)),
            (Some("2026-10"), household, 300, Some(PlanSpending::Budget)),
        ]),
        ..item(groceries, 300, every_months(1, date(2025, 1, 10)))
    }];

    let by_category: Vec<_> = report(&asked)
        .rows
        .iter()
        .map(|row| (row.month.to_string(), row.category, row.planned.minor_units))
        .collect();
    assert_eq!(
        by_category,
        [
            ("2026-09".into(), groceries, 300),
            ("2026-10".into(), household, 300),
        ]
    );
}

#[test]
fn a_budget_runs_in_the_category_it_had_that_month() {
    let groceries = CategoryId::new();
    let household = CategoryId::new();
    let mut asked = input(2);
    asked.from = month("2026-09");
    asked.today = date(2026, 10, 20);
    asked.items = vec![ReportItem {
        versions: versions(&[
            (None, groceries, 300, Some(PlanSpending::Budget)),
            (Some("2026-10"), household, 300, Some(PlanSpending::Budget)),
        ]),
        ..item(groceries, 300, every_months(1, date(2025, 1, 10)))
    }];

    let rollovers = report(&asked).rollovers;
    let budgeted: Vec<_> = rollovers
        .iter()
        .map(|row| (row.category, row.budgeted.minor_units))
        .collect();
    assert_eq!(budgeted.len(), 2, "one rollover per category budgeted");
    assert!(budgeted.contains(&(groceries, 300)));
    assert!(budgeted.contains(&(household, 300)));
}
