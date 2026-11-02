//! What this month's actual spending does to the plan (spec §6.8, §8.9 R9). Only
//! stored outflows are touched: a hypothetical item neither reduces, consumes nor
//! owes, and a goal has spending of its own.
//!
//! A recurring bill or a one-off counts in full until its date passes. Past it,
//! spending in its category pays them in date order, and whatever it does not cover
//! is still needed: a gap, reported, since the money may have gone under another
//! category. Spending goes to them before any budget, since the data cannot tell
//! which it was for, so money spent on a budget can hide an unpaid bill.
//!
//! A budget is reduced. Each category and currency has one pool: the month's
//! spending less every planned outflow dated earlier this month, bills first, since
//! that spending was already planned for, floored at 0. The pool then pays for this
//! month's remaining budget occurrences in date order, so one purchase never reduces
//! two items, and a paid bill never eats a budget.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::count;
use super::types::{ForecastError, ForecastInput, ItemInput, LineSource};
use crate::ids::CategoryId;
use crate::money::Currency;
use crate::plan::{PlanDirection, PlanSchedule, PlanSpending};

type PoolKey = (CategoryId, Currency);

/// One occurrence of an item, by its position in the input.
struct Occurrence {
    on: Date,
    index: usize,
    key: PoolKey,
    amount: i128,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ItemAdjustment {
    /// What comes off a budget in the window.
    pub(super) reduced_by: i128,
    /// What a recurring bill or one-off dated earlier this month still needs.
    pub(super) unpaid: i128,
}

pub(super) struct Reduction {
    /// One per item, in input order.
    pub(super) by_item: Vec<ItemAdjustment>,
    /// The gaps, per category and currency.
    pub(super) not_seen: BTreeMap<PoolKey, i128>,
    /// The currencies where more spending found this month could change the number:
    /// a budget still to reduce, or a gap it might close.
    pub(super) spending_sensitive_currencies: BTreeSet<Currency>,
    /// The currencies where spending paid an earlier bill or one-off: were part of it
    /// a duplicate, a gap would open.
    pub(super) paid_currencies: BTreeSet<Currency>,
}

/// A stored outflow's category, the only kind of item that shares a pool.
fn pool_key(item: &ItemInput) -> Option<PoolKey> {
    let stored = matches!(item.source, LineSource::PlannedItem { .. });
    let outflow = item.direction == PlanDirection::Outflow;
    let category = item.category.filter(|_| stored && outflow)?;
    Some((category, item.amount.currency))
}

fn occurrences(
    dates: Vec<Date>,
    index: usize,
    key: PoolKey,
    amount: i128,
) -> impl Iterator<Item = Occurrence> {
    dates.into_iter().map(move |on| Occurrence {
        on,
        index,
        key,
        amount,
    })
}

pub(super) fn reduce(
    input: &ForecastInput,
    end_exclusive: Date,
) -> Result<Reduction, ForecastError> {
    let month_start = input.as_of.first_of_month();
    let reducible_until = month_start
        .checked_add(Span::new().months(1))
        .map_or(end_exclusive, |next_month| next_month.min(end_exclusive));

    let mut spent: BTreeMap<PoolKey, i128> = BTreeMap::new();
    for row in &input.category_spent {
        *spent.entry((row.category, row.currency)).or_default() += i128::from(row.spent_minor);
    }

    let mut planned_earlier: BTreeMap<PoolKey, i128> = BTreeMap::new();
    let mut earlier_payments = Vec::new();
    let mut remaining_budgets = Vec::new();
    for (index, item) in input.items.iter().enumerate() {
        let Some(key) = pool_key(item) else {
            continue;
        };
        let amount = i128::from(item.amount.minor_units);
        let earlier = item.schedule.occurrences(month_start, input.as_of);
        *planned_earlier.entry(key).or_default() += amount * i128::from(count(&earlier)?);
        let budget = item.spending == Some(PlanSpending::Budget)
            && matches!(item.schedule, PlanSchedule::Recurring { .. });
        if budget {
            let dates = item.schedule.occurrences(input.as_of, reducible_until);
            remaining_budgets.extend(occurrences(dates, index, key, amount));
        } else {
            // A goal has no occurrences, so this is recurring bills and one-offs.
            earlier_payments.extend(occurrences(earlier, index, key, amount));
        }
    }

    let mut by_item = vec![ItemAdjustment::default(); input.items.len()];
    let Payments {
        not_seen,
        paid_currencies,
    } = pay_earlier_payments(earlier_payments, &spent, &mut by_item);

    let mut spending_sensitive_currencies: BTreeSet<Currency> =
        not_seen.keys().map(|&(_, currency)| currency).collect();
    spending_sensitive_currencies.extend(remaining_budgets.iter().map(|one| one.key.1));

    let mut pools = spent;
    for (key, planned) in planned_earlier {
        if let Some(pool) = pools.get_mut(&key) {
            *pool -= planned;
        }
    }
    reduce_budgets(remaining_budgets, pools, &mut by_item);

    Ok(Reduction {
        by_item,
        not_seen,
        spending_sensitive_currencies,
        paid_currencies,
    })
}

struct Payments {
    not_seen: BTreeMap<PoolKey, i128>,
    paid_currencies: BTreeSet<Currency>,
}

/// Pays recurring bills and one-offs from their category's spending, earliest first
/// and in input order on the same day, and returns what is left unpaid per category
/// and currency.
fn pay_earlier_payments(
    mut payments: Vec<Occurrence>,
    spent: &BTreeMap<PoolKey, i128>,
    by_item: &mut [ItemAdjustment],
) -> Payments {
    payments.sort_by_key(|payment| payment.on);
    let mut left = spent.clone();
    let mut not_seen = BTreeMap::new();
    let mut paid_currencies = BTreeSet::new();
    for payment in payments {
        let available = left.entry(payment.key).or_default();
        let paid = payment.amount.min((*available).max(0));
        *available -= paid;
        if paid != 0 {
            paid_currencies.insert(payment.key.1);
        }
        let unpaid = payment.amount - paid;
        if unpaid != 0 {
            *not_seen.entry(payment.key).or_default() += unpaid;
            if let Some(item) = by_item.get_mut(payment.index) {
                item.unpaid += unpaid;
            }
        }
    }
    Payments {
        not_seen,
        paid_currencies,
    }
}

/// Spends each pool on its budgets, earliest first and in input order on the same
/// day. A pool below 0 is empty.
fn reduce_budgets(
    mut budgets: Vec<Occurrence>,
    mut pools: BTreeMap<PoolKey, i128>,
    by_item: &mut [ItemAdjustment],
) {
    budgets.sort_by_key(|budget| budget.on);
    for budget in budgets {
        let (Some(pool), Some(item)) = (pools.get_mut(&budget.key), by_item.get_mut(budget.index))
        else {
            continue;
        };
        let taken = budget.amount.min((*pool).max(0));
        *pool -= taken;
        item.reduced_by += taken;
    }
}
