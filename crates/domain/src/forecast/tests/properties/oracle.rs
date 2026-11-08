//! The oracles the properties check a forecast against: aggregates worked out apart from the
//! forecast's own walk.

use std::collections::{BTreeMap, BTreeSet};

use jiff::ToSpan;
use jiff::civil::Date;

use super::{wide, window_end};
use crate::forecast::{
    BILL_EARLY_PAYMENT_DAYS, Caution, Forecast, ForecastInput, ItemInput, ItemLine, LineSource,
    bills_looked_at_from,
};
use crate::ids::CategoryId;
use crate::money::Currency;
use crate::plan::{CalendarMonth, PlanDirection, PlanSchedule, PlanSpending, SettlementKind};

/// One category and currency's figures, worked out here in aggregate.
#[derive(Default)]
struct PoolFigures {
    spent: i128,
    /// Recurring bills and one-offs from the start of the look-back up to `as_of`.
    earlier_bills: i128,
    /// A settlement says it was paid, which can take a transaction out of the pool unseen.
    settled_paid: bool,
    /// Budgets' occurrences anywhere in the month.
    month_budgets: i128,
    /// Budgets' occurrences in the window after the month.
    later_budgets: i128,
}

/// A stored budget, the only item whose window reaches back to the month's start.
pub(super) fn is_stored_budget(item: &ItemInput) -> bool {
    matches!(item.source, LineSource::PlannedItem { .. })
        && item.direction == PlanDirection::Outflow
        && item.category.is_some()
        && item.spending == Some(PlanSpending::Budget)
}

/// What `item` counts for in `[from, to)`: a settled month's occurrences count as nothing.
pub(super) fn dated(item: &ItemInput, from: Date, to: Date) -> Result<i128, String> {
    let mut dates = 0_i128;
    for on in item.schedule.occurrences(from, to) {
        let month = CalendarMonth::of(on).map_err(|_| "no month")?;
        if !item.settled.contains_key(&month) {
            dates += 1;
        }
    }
    Ok(i128::from(item.amount.minor_units) * dates)
}

/// What `item` would count for in `[from, to)` were nothing settled, less [`dated`]: the
/// settled occurrences' whole amount.
pub(super) fn settled_amount(item: &ItemInput, from: Date, to: Date) -> Result<i128, String> {
    let dates = item.schedule.occurrences(from, to).len();
    let dates = i128::try_from(dates).map_err(|_| "too many")?;
    Ok(i128::from(item.amount.minor_units) * dates - dated(item, from, to)?)
}

/// The list says exactly what was taken out, item by item: the settled occurrences from the
/// first day the look-back reaches to the window's end, and nothing with none.
pub(super) fn check_settled_list(input: &ForecastInput, forecast: &Forecast) -> Result<(), String> {
    let looked_at_from = bills_looked_at_from(input.as_of);
    let window_ends = window_end(input.as_of, input.horizon_months)?;
    for item in &input.items {
        let listed: i128 = forecast
            .settled
            .iter()
            .filter(|line| line.source == item.source)
            .map(|line| wide(line.amount))
            .sum();
        if listed != settled_amount(item, looked_at_from, window_ends)? {
            return Err("the settled list is not what was taken out".to_owned());
        }
    }
    if forecast.settled.iter().any(|line| line.occurrences == 0) {
        return Err("a settled line with no occurrence".to_owned());
    }
    Ok(())
}

/// `input` as it would be had no one settled anything.
pub(super) fn unsettled(input: &ForecastInput) -> ForecastInput {
    let mut clear = input.clone();
    for item in &mut clear.items {
        item.settled.clear();
    }
    clear
}

/// What the payments on `lines` say: a transaction pays one occurrence at most, in tolerance.
pub(super) fn check_payments(forecast: &Forecast) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for payment in forecast.items.iter().flat_map(|line| &line.payments) {
        let Some(paid_by) = payment.paid_by else {
            continue;
        };
        if !seen.insert(paid_by.transaction) {
            return Err("a transaction paid two occurrences".to_owned());
        }
        if (wide(paid_by.amount) - wide(payment.amount)).abs() * 100 > wide(payment.amount) * 10 {
            return Err("a payment is out of tolerance".to_owned());
        }
    }
    Ok(())
}

/// `item`'s unsettled occurrences from the look-back to `as_of`, but for last month's before history.
pub(super) fn evidenced(input: &ForecastInput, item: &ItemInput) -> Result<i128, String> {
    let month_start = input.as_of.first_of_month();
    let from = input.earliest_transaction.map_or(month_start, |first| {
        first
            .max(bills_looked_at_from(input.as_of))
            .min(month_start)
    });
    dated(item, from, input.as_of)
}

/// What a category's lines owe is its bills before `as_of` less those paid, and this month's part
/// of it is what `bill_not_seen` says.
pub(super) fn check_owed(
    input: &ForecastInput,
    forecast: &Forecast,
    (category, code): (CategoryId, Currency),
    earlier_bills: i128,
) -> Result<(), String> {
    let lines = || {
        forecast
            .items
            .iter()
            .filter(|line| line.category == Some(category) && line.gross.currency == code)
    };
    let before_as_of = || {
        lines()
            .flat_map(|line| &line.payments)
            .filter(|payment| payment.on < input.as_of)
    };
    let unpaid: i128 = lines().map(|line| wide(line.unpaid)).sum();
    let paid: i128 = before_as_of()
        .filter(|payment| payment.paid_by.is_some())
        .map(|payment| wide(payment.amount))
        .sum();
    if unpaid != earlier_bills - paid {
        return Err("what a bill owes is wrong".to_owned());
    }
    let warned: i128 = forecast
        .cautions
        .iter()
        .filter_map(|caution| match caution {
            Caution::BillNotSeen {
                category: found,
                amount,
                ..
            } if *found == category && amount.currency == code => Some(wide(*amount)),
            _ => None,
        })
        .sum();
    let this_month: i128 = before_as_of()
        .filter(|payment| payment.paid_by.is_none() && !payment.before_history)
        .filter(|payment| payment.on >= input.as_of.first_of_month())
        .map(|payment| wide(payment.amount))
        .sum();
    if warned != this_month {
        return Err("a gap was not reported in full".to_owned());
    }
    Ok(())
}

/// Checks reductions and unpaid bills against aggregates worked out apart from the
/// walk: the pool (spending less what paid a bill this month, floored at 0) is either all used or
/// covers the month's budgets, and what is unpaid is the bills dated before `as_of` less those
/// paid. What budgets count is then what the month's leave plus the later months' whole.
pub(super) fn check_reduction(input: &ForecastInput, forecast: &Forecast) -> Result<(), String> {
    let month_start = input.as_of.first_of_month();
    let window_ends = window_end(input.as_of, input.horizon_months)?;
    let month_end = input
        .as_of
        .last_of_month()
        .tomorrow()
        .map_err(|_| "no next month")?
        .min(window_ends);
    let mut pools: BTreeMap<(CategoryId, Currency), PoolFigures> = BTreeMap::new();
    for spent in &input.category_spent {
        pools
            .entry((spent.category, spent.currency))
            .or_default()
            .spent += i128::from(spent.spent_minor);
    }
    for item in &input.items {
        let stored_outflow = matches!(item.source, LineSource::PlannedItem { .. })
            && item.direction == PlanDirection::Outflow;
        let (true, Some(category)) = (stored_outflow, item.category) else {
            continue;
        };
        let figures = pools.entry((category, item.amount.currency)).or_default();
        if item.spending == Some(PlanSpending::Budget) {
            figures.month_budgets += dated(item, month_start, month_end)?;
            figures.later_budgets += dated(item, month_end, window_ends)?;
        } else {
            // A goal has no occurrences, so this is recurring bills and one-offs.
            figures.earlier_bills += evidenced(input, item)?;
            figures.settled_paid |= item
                .settled
                .values()
                .any(|&kind| kind == SettlementKind::Paid);
        }
    }
    for (&(category, code), figures) in &pools {
        let lines = || {
            forecast
                .items
                .iter()
                .filter(move |line| line.category == Some(category) && line.gross.currency == code)
        };
        check_owed(input, forecast, (category, code), figures.earlier_bills)?;
        let reduced: i128 = lines().map(|line| wide(line.reduced_by)).sum();
        let payments = || lines().flat_map(|line| &line.payments);
        let paid_this_month: i128 = payments()
            .filter_map(|payment| payment.paid_by)
            .filter(|paid_by| paid_by.posted_on >= month_start)
            .map(|paid_by| wide(paid_by.amount))
            .sum();
        let pool = (figures.spent - paid_this_month).max(0);
        let reduced_expected = pool.min(figures.month_budgets);
        // Unclear money leaves the pool beyond the bill, which `payments` doesn't show.
        let unclear = forecast.cautions.iter().any(|caution| {
            matches!(caution, Caution::BillPaymentUnclear { amount, .. } if amount.currency == code)
        });
        if figures.settled_paid || unclear {
            if reduced > reduced_expected {
                return Err("a pool was spent more than once".to_owned());
            }
            continue;
        }
        if reduced != reduced_expected {
            return Err("a pool was not spent exactly once".to_owned());
        }
        let budgets_counted: i128 = input
            .items
            .iter()
            .filter(|item| {
                is_stored_budget(item)
                    && item.category == Some(category)
                    && item.amount.currency == code
            })
            .flat_map(|item| {
                forecast
                    .items
                    .iter()
                    .filter(|line| line.source == item.source)
            })
            .map(|line| wide(line.counted))
            .sum();
        if budgets_counted != figures.month_budgets - reduced_expected + figures.later_budgets {
            return Err("a budget's month was not held back".to_owned());
        }
    }
    check_payments(forecast)?;
    check_budget_months(input, forecast, month_start, month_end)
}

/// What `line` says payments already made paid of `item`'s occurrences dated `as_of` or later
/// (spec §10.8), each checked apart from the walk, and that only today's is listed unpaid: an unsettled occurrence of the item no more
/// than [`BILL_EARLY_PAYMENT_DAYS`] after `as_of`, paid by a row posted by `as_of` and no more
/// than that many days before the occurrence. Tolerance and one row per occurrence are
/// [`check_payments`]'s.
pub(super) fn paid_ahead(
    input: &ForecastInput,
    item: &ItemInput,
    line: &ItemLine,
) -> Result<i128, String> {
    let early_until = input
        .as_of
        .checked_add(BILL_EARLY_PAYMENT_DAYS.days())
        .map_err(|_| "no early window")?;
    let mut paid = 0_i128;
    for payment in line
        .payments
        .iter()
        .filter(|payment| payment.on >= input.as_of)
    {
        // Today's is listed paid or not; a later one only once paid.
        let Some(paid_by) = payment.paid_by else {
            if payment.on > input.as_of {
                return Err("an occurrence not yet due is listed unpaid".to_owned());
            }
            continue;
        };
        let earliest = payment
            .on
            .checked_sub(BILL_EARLY_PAYMENT_DAYS.days())
            .map_err(|_| "no earliest day")?;
        let next = payment.on.tomorrow().map_err(|_| "no next day")?;
        let month = CalendarMonth::of(payment.on).map_err(|_| "no month")?;
        let occurs = item.schedule.occurrences(payment.on, next) == [payment.on];
        if payment.on > early_until
            || paid_by.posted_on > input.as_of
            || paid_by.posted_on < earliest
            || !occurs
            || item.settled.contains_key(&month)
        {
            return Err("an occurrence was paid ahead by a payment that can't pay it".to_owned());
        }
        paid += wide(payment.amount);
    }
    Ok(paid)
}

/// The first bucket holds all of what a stored budget's month leaves, and each line's
/// `gross` is its occurrences from the month's start for a stored budget, or from `as_of`
/// for anything else, less those a payment already paid.
pub(super) fn check_budget_months(
    input: &ForecastInput,
    forecast: &Forecast,
    month_start: Date,
    month_end: Date,
) -> Result<(), String> {
    let first = forecast.months.first().ok_or("no first month")?;
    let window_ends = window_end(input.as_of, input.horizon_months)?;
    for item in &input.items {
        let dated_item = !matches!(item.schedule, PlanSchedule::Goal { .. });
        let outflow = item.direction == PlanDirection::Outflow;
        let line = forecast
            .items
            .iter()
            .find(|line| line.source == item.source);
        if !(dated_item && outflow && matches!(item.source, LineSource::PlannedItem { .. })) {
            continue;
        }
        let from = if is_stored_budget(item) {
            month_start
        } else {
            input.as_of
        };
        let paid = match line {
            Some(line) if !is_stored_budget(item) => paid_ahead(input, item, line)?,
            _ => 0,
        };
        if line.map_or(0, |line| wide(line.gross)) != dated(item, from, window_ends)? - paid {
            return Err("a line's gross has the wrong occurrences".to_owned());
        }
        if is_stored_budget(item) {
            let reduced = line.map_or(0, |line| wide(line.reduced_by));
            let in_first: i128 = first
                .lines
                .iter()
                .filter(|month| month.currency == item.amount.currency)
                .flat_map(|month| &month.items)
                .filter(|listed| listed.source == item.source)
                .map(|listed| wide(listed.amount))
                .sum();
            if in_first != dated(item, month_start, month_end)? - reduced {
                return Err("the first month does not hold a budget's month".to_owned());
            }
        }
    }
    Ok(())
}
