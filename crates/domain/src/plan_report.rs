//! Plan versus actual (spec §6.9, §8.9 R6, R21): per category and month, what today's
//! plan says against what happened. Pure and exact: amounts add up in `i128`, and any
//! total that doesn't fit in `i64` refuses the answer.
//!
//! What `actual` holds, so that money is never silently absent:
//!
//! - Money a flagged transaction would add to `actual`, a charge in an `expense`
//!   category or a deposit in an `income` one, stays out of it when the provider removed
//!   the transaction or revised it away from its splits, and its row's `left_out` says
//!   how much. Money that lowers `actual`, such as a refund, keeps counting.
//! - A possible duplicate keeps counting, since it may be real, and its row's
//!   `possible_duplicate` says how much of `actual` it is.
//! - A split linked to a goal counts on the goal's line and never in its category.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;
use serde::Serialize;

use crate::forecast::{ForecastError, money};
use crate::ids::{CategoryId, PlannedItemId};
use crate::money::{Currency, Money};
use crate::plan::{CalendarMonth, PlanSchedule, PlanSpending, div_half_even, goal_accrued};
use crate::transactions::CategoryKind;

/// Everything one report reads, gathered by the caller at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReportInput {
    /// The first month reported.
    pub from: CalendarMonth,
    /// How many months are reported.
    pub months: u8,
    /// How many calendar months each rolling average covers.
    pub average_months: u8,
    /// The household's day.
    pub today: Date,
    pub items: Vec<ReportItem>,
    /// Per month, category and currency, as far back as [`report_window`] says.
    pub actuals: Vec<MonthActual>,
    pub uncategorized: Vec<MonthUncategorized>,
}

/// A planned item as a report reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportItem {
    pub id: PlannedItemId,
    pub name: String,
    pub category: CategoryId,
    /// A positive magnitude.
    pub amount: Money,
    pub schedule: PlanSchedule,
    /// A budget has a rollover; a bill doesn't.
    pub spending: Option<PlanSpending>,
    /// An inactive item plans and accrues nothing, but money linked to an inactive goal
    /// keeps its line, since it counts nowhere else.
    pub active: bool,
    /// What is linked to this goal, per month.
    pub goal_spent: Vec<GoalMonth>,
}

/// The money linked to one goal in one month, positive for money spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalMonth {
    pub month: CalendarMonth,
    /// Net of refunds, so a month of refunds is negative.
    pub spent_minor: i64,
    /// Charges that don't count against the goal: removed, revised or possibly
    /// duplicates (R10a).
    pub left_out_minor: i64,
}

/// A category's splits not linked to a goal, in one month and currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthActual {
    pub month: CalendarMonth,
    pub category: CategoryId,
    pub kind: CategoryKind,
    pub currency: Currency,
    /// The sum of the counted splits, negative for money out.
    pub net_minor: i64,
    /// The magnitude of what stays out of `net_minor`.
    pub left_out_minor: i64,
    /// The magnitude of what `net_minor` holds from possible duplicates.
    pub possible_duplicate_minor: i64,
}

/// Outflows with no splits on spendable accounts, in one month and currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthUncategorized {
    pub month: CalendarMonth,
    pub currency: Currency,
    pub count: u32,
    /// Positive for money out.
    pub total_minor: i64,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlanReport {
    /// Ordered by month, category and currency. A row with nothing to say is left out.
    pub rows: Vec<PlanRow>,
    /// Ordered by month and goal.
    pub goals: Vec<GoalRow>,
    /// Ordered by month and currency. Never spending.
    pub uncategorized: Vec<UncategorizedRow>,
    /// What each category with a budget has left, one line per category and currency,
    /// ordered by both.
    pub rollovers: Vec<RolloverRow>,
    /// The reported month holding today, whose figures aren't final.
    pub partial_months: Vec<CalendarMonth>,
}

/// One category in one month and currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlanRow {
    pub month: CalendarMonth,
    pub category: CategoryId,
    pub currency: Currency,
    /// The occurrences of today's active items that fall in this month.
    pub planned: Money,
    /// Spending in an `expense` category or income in an `income` one, net, so a month
    /// of refunds is negative.
    pub actual: Money,
    /// `planned` minus `actual`.
    pub difference: Money,
    /// The mean of `actual` over the window ending this month, zeros included, rounded
    /// half-even.
    pub rolling_average: Money,
    /// Money left out of `actual` because its provider removed the transaction or
    /// revised it away from its splits.
    pub left_out: Money,
    /// How much of `actual` is on transactions flagged as possible duplicates.
    pub possible_duplicate: Money,
}

/// One goal in one month.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GoalRow {
    pub month: CalendarMonth,
    pub goal: PlannedItemId,
    pub name: String,
    pub currency: Currency,
    /// What the goal's schedule sets aside this month.
    pub accrued: Money,
    /// What was spent on it, net, so a month of refunds is negative.
    pub spent_against: Money,
    /// Charges left out of `spent_against`: removed, revised or possibly duplicates.
    pub left_out: Money,
}

/// Outflows no one has categorized, in one month and currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UncategorizedRow {
    pub month: CalendarMonth,
    pub currency: Currency,
    pub count: u32,
    pub total: Money,
}

/// A category's budgets over the reported months up to today's: bills have no rollover,
/// and a month yet to come has left nothing unspent. It covers those months only, so moving
/// `from` changes it, and only this report carries it: the forecast doesn't carry leftovers
/// over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RolloverRow {
    pub category: CategoryId,
    pub currency: Currency,
    /// The occurrences of the category's budgets in those months.
    pub budgeted: Money,
    /// What those months spent on them, while a budget was running: each month's `actual`
    /// once the bills and one-offs dated before today have taken their share. A month of
    /// refunds lowers it.
    pub spent: Money,
    /// `budgeted` minus `spent`: positive for what was left unspent, negative for
    /// overspent.
    pub rollover: Money,
}

/// The days whose actuals a report reads: from the first day of the earliest month any
/// rolling average covers, up to, not including, the day after the last reported month.
///
/// # Errors
/// Returns [`ForecastError::OutOfRange`] when the window leaves the calendar.
pub fn report_window(
    from: CalendarMonth,
    months: u8,
    average_months: u8,
) -> Result<(Date, Date), ForecastError> {
    let months_after = |count: i64| {
        from.first_day()
            .checked_add(Span::new().months(count))
            .map_err(|_| ForecastError::OutOfRange)
    };
    let earlier = i64::from(average_months.max(1)) - 1;
    Ok((months_after(-earlier)?, months_after(i64::from(months))?))
}

/// Computes the report for `input`, which the caller has validated.
///
/// # Errors
/// Returns [`ForecastError::OutOfRange`] when a total does not fit in `i64`, or a month
/// leaves the calendar.
pub fn plan_vs_actual(input: &PlanReportInput) -> Result<PlanReport, ForecastError> {
    let today_month = CalendarMonth::of(input.today).map_err(|_| ForecastError::OutOfRange)?;
    let average_months = usize::from(input.average_months.max(1));
    let (start, _) = report_window(input.from, input.months, input.average_months)?;
    // By count: `CalendarMonth::next` stays on the calendar's last month.
    let mut all_months = vec![CalendarMonth::of(start).map_err(|_| ForecastError::OutOfRange)?];
    for _ in 1..(average_months - 1 + usize::from(input.months)) {
        let last = all_months[all_months.len() - 1];
        all_months.push(last.next());
    }
    let reported = &all_months[average_months - 1..];

    let planned = planned_by_month(input, reported);
    let actuals = actuals_by_month(&input.actuals);
    let mut rows = Vec::new();
    let pairs: BTreeSet<Pair> = planned
        .keys()
        .chain(actuals.keys())
        .map(|&(_, category, currency)| (category, currency))
        .collect();
    for (index, &month) in reported.iter().enumerate() {
        let window = &all_months[index..index + average_months];
        for &pair in &pairs {
            if let Some(row) = plan_row(month, pair, window, &planned, &actuals)? {
                rows.push(row);
            }
        }
    }
    let up_to_today: Vec<CalendarMonth> = reported
        .iter()
        .copied()
        .filter(|&month| month <= today_month)
        .collect();
    Ok(PlanReport {
        rows,
        goals: goal_rows(input, reported)?,
        uncategorized: input
            .uncategorized
            .iter()
            .filter(|row| reported.contains(&row.month))
            .map(|row| UncategorizedRow {
                month: row.month,
                currency: row.currency,
                count: row.count,
                total: Money {
                    minor_units: row.total_minor,
                    currency: row.currency,
                },
            })
            .collect(),
        rollovers: rollover_rows(input, &pairs, &up_to_today, &planned, &actuals)?,
        partial_months: reported
            .iter()
            .copied()
            .filter(|&month| month == today_month)
            .collect(),
    })
}

/// A category in one currency.
type Pair = (CategoryId, Currency);
/// A category in one currency and month.
type Key = (CalendarMonth, CategoryId, Currency);

/// What the plan says of one [`Key`].
#[derive(Debug, Clone, Copy, Default)]
struct Planned {
    total: i128,
    /// The part of `total` from budgets.
    budget: i128,
    /// The bills and one-offs dated before today, which spending pays first (R9).
    due_before_today: i128,
}

/// What happened in one [`Key`], with `actual` positive for what the category's kind counts.
#[derive(Debug, Clone, Copy, Default)]
struct Happened {
    actual: i128,
    left_out: i128,
    possible_duplicate: i128,
}

fn planned_by_month(input: &PlanReportInput, reported: &[CalendarMonth]) -> BTreeMap<Key, Planned> {
    let mut planned: BTreeMap<Key, Planned> = BTreeMap::new();
    for item in input.items.iter().filter(|item| item.active) {
        let amount = i128::from(item.amount.minor_units);
        let is_budget = item.spending == Some(PlanSpending::Budget);
        for &month in reported {
            for on in item
                .schedule
                .occurrences(month.first_day(), month.next().first_day())
            {
                let entry = planned
                    .entry((month, item.category, item.amount.currency))
                    .or_default();
                entry.total += amount;
                if is_budget {
                    entry.budget += amount;
                } else if on < input.today {
                    entry.due_before_today += amount;
                }
            }
        }
    }
    planned
}

/// `actuals` as the report counts them: a transfer isn't spending or income.
fn actuals_by_month(actuals: &[MonthActual]) -> BTreeMap<Key, Happened> {
    let mut found: BTreeMap<Key, Happened> = BTreeMap::new();
    for row in actuals {
        let net = i128::from(row.net_minor);
        let actual = match row.kind {
            CategoryKind::Expense => -net,
            CategoryKind::Income => net,
            CategoryKind::Transfer => continue,
        };
        let entry = found
            .entry((row.month, row.category, row.currency))
            .or_default();
        entry.actual += actual;
        entry.left_out += i128::from(row.left_out_minor);
        entry.possible_duplicate += i128::from(row.possible_duplicate_minor);
    }
    found
}

/// The row of `month`, unless it has nothing to say. `window` is the months its rolling
/// average covers.
fn plan_row(
    month: CalendarMonth,
    (category, currency): Pair,
    window: &[CalendarMonth],
    planned: &BTreeMap<Key, Planned>,
    actuals: &BTreeMap<Key, Happened>,
) -> Result<Option<PlanRow>, ForecastError> {
    let key = (month, category, currency);
    let plan = planned.get(&key).copied().unwrap_or_default();
    let happened = actuals.get(&key).copied().unwrap_or_default();
    let window_total: i128 = window
        .iter()
        .filter_map(|&month| actuals.get(&(month, category, currency)))
        .map(|happened| happened.actual)
        .sum();
    let months_in_window = i128::try_from(window.len()).map_err(|_| ForecastError::OutOfRange)?;
    let rolling_average = div_half_even(window_total, months_in_window);
    if plan.total == 0
        && happened.actual == 0
        && rolling_average == 0
        && happened.left_out == 0
        && happened.possible_duplicate == 0
    {
        return Ok(None);
    }
    Ok(Some(PlanRow {
        month,
        category,
        currency,
        planned: money(plan.total, currency)?,
        actual: money(happened.actual, currency)?,
        difference: money(plan.total - happened.actual, currency)?,
        rolling_average: money(rolling_average, currency)?,
        left_out: money(happened.left_out, currency)?,
        possible_duplicate: money(happened.possible_duplicate, currency)?,
    }))
}

/// Each goal's lines, by month and in the order goals come in.
fn goal_rows(
    input: &PlanReportInput,
    reported: &[CalendarMonth],
) -> Result<Vec<GoalRow>, ForecastError> {
    let mut rows = Vec::new();
    for &month in reported {
        for item in &input.items {
            let PlanSchedule::Goal {
                start_on, due_on, ..
            } = item.schedule
            else {
                continue;
            };
            let accrued_at =
                |at: Date| i128::from(goal_accrued(item.amount.minor_units, start_on, due_on, at));
            let accrued = if item.active {
                accrued_at(month.next().first_day()) - accrued_at(month.first_day())
            } else {
                0
            };
            let (spent, left_out) = item
                .goal_spent
                .iter()
                .find(|linked| linked.month == month)
                .map_or((0, 0), |linked| (linked.spent_minor, linked.left_out_minor));
            if accrued == 0 && spent == 0 && left_out == 0 {
                continue;
            }
            let currency = item.amount.currency;
            rows.push(GoalRow {
                month,
                goal: item.id,
                name: item.name.clone(),
                currency,
                accrued: money(accrued, currency)?,
                spent_against: money(i128::from(spent), currency)?,
                left_out: money(i128::from(left_out), currency)?,
            });
        }
    }
    Ok(rows)
}

/// A line for each of `pairs` with a budget falling in `months`. A month's spending counts
/// only while a budget in the pair is running, so what came before or after it isn't
/// charged to it.
fn rollover_rows(
    input: &PlanReportInput,
    pairs: &BTreeSet<Pair>,
    months: &[CalendarMonth],
    planned: &BTreeMap<Key, Planned>,
    actuals: &BTreeMap<Key, Happened>,
) -> Result<Vec<RolloverRow>, ForecastError> {
    let mut budgets: BTreeMap<Pair, Vec<PlanSchedule>> = BTreeMap::new();
    for item in input
        .items
        .iter()
        .filter(|item| item.active && item.spending == Some(PlanSpending::Budget))
    {
        budgets
            .entry((item.category, item.amount.currency))
            .or_default()
            .push(item.schedule);
    }
    let mut rows = Vec::new();
    for &pair @ (category, currency) in pairs {
        let schedules = budgets.get(&pair).map_or(&[][..], Vec::as_slice);
        let (mut budgeted, mut spent) = (0, 0);
        for &month in months {
            let key = (month, category, currency);
            let plan = planned.get(&key).copied().unwrap_or_default();
            budgeted += plan.budget;
            if !schedules.iter().any(|schedule| runs_in(schedule, month)) {
                continue;
            }
            let actual = actuals.get(&key).map_or(0, |happened| happened.actual);
            spent += if actual > 0 {
                (actual - plan.due_before_today).max(0)
            } else {
                actual
            };
        }
        if budgeted > 0 {
            rows.push(RolloverRow {
                category,
                currency,
                budgeted: money(budgeted, currency)?,
                spent: money(spent, currency)?,
                rollover: money(budgeted - spent, currency)?,
            });
        }
    }
    Ok(rows)
}

/// Whether a budget on `schedule` is running in `month`: from the month of its first
/// occurrence through the month of its end date, whether or not an occurrence falls there.
fn runs_in(schedule: &PlanSchedule, month: CalendarMonth) -> bool {
    let first_day = month.first_day();
    match *schedule {
        PlanSchedule::Recurring {
            anchor_on, end_on, ..
        } => anchor_on.first_of_month() <= first_day && end_on.is_none_or(|end| first_day <= end),
        PlanSchedule::OneOff { .. } | PlanSchedule::Goal { .. } => false,
    }
}

#[cfg(test)]
mod tests;
