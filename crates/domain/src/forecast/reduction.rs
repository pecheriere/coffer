//! What actual spending does to the plan (spec §6.8, §8.9 R9, R9b). Only stored outflows are
//! touched: a hypothetical item neither reduces, consumes nor owes, and a goal has spending of
//! its own.
//!
//! A recurring bill or a one-off counts in full until its date passes, so one dated `as_of`
//! counts in full even if it was paid that day. Past it, and back through last month, it is
//! paid only by a transaction [`bill_matching`](super::bill_matching) matches to it: whatever
//! isn't paid is still needed, and reported, since the money may have gone under another
//! category.
//!
//! A budget is reduced, and it counts its whole calendar month: all its occurrences in
//! `as_of`'s month, before `as_of` too, go into its line and the first bucket, so what it
//! has not spent is still needed. Later months count only inside the window. Each
//! category and currency has one pool: the month's spending less what paid a bill, floored at
//! 0. The pool then pays for the month's budget occurrences in date order, so one purchase never
//! reduces two items, and a paid bill never eats a budget.
//!
//! A settled occurrence (R9a) is out of all of this: it owes no gap and takes no spending.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::bill_matching::{Bill, Seen, bills_looked_at_from, match_bills};
use super::money;
use super::settled::settlement_of;
use super::types::{
    Caution, ForecastError, ForecastInput, ItemInput, LineSource, OccurrencePayment, PaidBy,
};
use crate::ids::CategoryId;
use crate::money::Currency;
use crate::plan::{PlanDirection, PlanSchedule, PlanSpending};

pub(super) type PoolKey = (CategoryId, Currency);

/// One occurrence of an item, by its position in the input.
struct Occurrence {
    on: Date,
    index: usize,
    key: PoolKey,
    amount: i128,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ItemAdjustment {
    /// What comes off a budget's occurrences this month.
    pub(super) reduced_by: i128,
    /// What a recurring bill or one-off dated before `as_of` still needs.
    pub(super) unpaid: i128,
    /// What the transactions say of each occurrence the look-back reaches, by date.
    pub(super) payments: Vec<OccurrencePayment>,
}

pub(super) struct Reduction {
    /// One per item, in input order.
    pub(super) by_item: Vec<ItemAdjustment>,
    /// The gaps dated this month, per category and currency.
    pub(super) not_seen: BTreeMap<PoolKey, i128>,
    /// What to say of a single occurrence: unpaid from last month, or unclear.
    pub(super) bill_cautions: Vec<Caution>,
    /// The currencies where more spending found this month could change the number:
    /// a budget still to reduce, or a gap it might close.
    pub(super) spending_sensitive_currencies: BTreeSet<Currency>,
}

/// A stored outflow's category, the only kind of item that shares a pool.
pub(super) fn pool_key(item: &ItemInput) -> Option<PoolKey> {
    let stored = matches!(item.source, LineSource::PlannedItem { .. });
    let outflow = item.direction == PlanDirection::Outflow;
    let category = item.category.filter(|_| stored && outflow)?;
    Some((category, item.amount.currency))
}

/// A stored budget, which counts its whole month. Only a recurring outflow is one, so a
/// one-off marked as a budget stays what it is.
pub(super) fn is_stored_budget(item: &ItemInput) -> bool {
    pool_key(item).is_some()
        && item.spending == Some(PlanSpending::Budget)
        && matches!(item.schedule, PlanSchedule::Recurring { .. })
}

/// The first day `item`'s occurrences count from. An inflow counts from the day after
/// `as_of`: a balance dated `as_of` may already hold that day's money, so counting it
/// could count it twice, and if it hasn't arrived the surplus reads low, the safe direction.
pub(super) fn counts_from(item: &ItemInput, as_of: Date) -> Date {
    if item.direction == PlanDirection::Inflow {
        as_of.tomorrow().unwrap_or(Date::MAX)
    } else if is_stored_budget(item) {
        as_of.first_of_month()
    } else {
        as_of
    }
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

    let mut pools: BTreeMap<PoolKey, i128> = BTreeMap::new();
    for row in &input.category_spent {
        *pools.entry((row.category, row.currency)).or_default() += i128::from(row.spent_minor);
    }

    let looked_at_from = bills_looked_at_from(input.as_of);
    let through_as_of = input.as_of.tomorrow().unwrap_or(Date::MAX);
    let mut bills = Vec::new();
    let mut month_budgets = Vec::new();
    for (index, item) in input.items.iter().enumerate() {
        let Some(key) = pool_key(item) else {
            continue;
        };
        let amount = i128::from(item.amount.minor_units);
        if is_stored_budget(item) {
            let dates = item.schedule.occurrences(month_start, reducible_until);
            month_budgets.extend(occurrences(dates, index, key, amount));
        } else {
            // A goal has no occurrences, so this is recurring bills and one-offs. One dated
            // `as_of` may be what today's spending paid, so it can take a transaction, but it
            // has not passed, so it owes no gap.
            let dates = item.schedule.occurrences(looked_at_from, through_as_of);
            bills.extend(dates.into_iter().map(|on| Bill {
                on,
                index,
                key,
                amount,
                settled: settlement_of(item, on),
            }));
        }
    }
    bills.sort_by_key(|bill| (bill.on, bill.index));

    let matching = match_bills(&bills, input);
    let mut by_item = vec![ItemAdjustment::default(); input.items.len()];
    let mut outcome = Outcome::default();
    for (bill, seen) in bills.iter().zip(&matching.seen) {
        if bill.settled.is_none()
            && let (Some(item), Some(adjustment)) =
                (input.items.get(bill.index), by_item.get_mut(bill.index))
        {
            record(input, bill, *seen, item, adjustment, &mut outcome)?;
        }
    }

    for (&at, &amount) in &matching.taken {
        if let Some(row) = input
            .spent_transactions
            .get(at)
            .filter(|row| row.posted_on >= month_start)
            && let Some(pool) = pools.get_mut(&(row.category, row.currency))
        {
            *pool -= amount;
        }
    }

    let Outcome {
        not_seen,
        bill_cautions,
        mut spending_sensitive_currencies,
    } = outcome;
    spending_sensitive_currencies.extend(month_budgets.iter().map(|one| one.key.1));
    reduce_budgets(month_budgets, pools, &mut by_item);

    Ok(Reduction {
        by_item,
        not_seen,
        bill_cautions,
        spending_sensitive_currencies,
    })
}

/// What the occurrences add up to, across items.
#[derive(Default)]
struct Outcome {
    not_seen: BTreeMap<PoolKey, i128>,
    bill_cautions: Vec<Caution>,
    spending_sensitive_currencies: BTreeSet<Currency>,
}

/// Enters what the transactions say of one unsettled occurrence. One dated `as_of` owes nothing
/// yet, whatever they say: it counts in full, from the window.
fn record(
    input: &ForecastInput,
    bill: &Bill,
    seen: Seen,
    item: &ItemInput,
    adjustment: &mut ItemAdjustment,
    outcome: &mut Outcome,
) -> Result<(), ForecastError> {
    let currency = bill.key.1;
    let row_at = |at: usize| input.spent_transactions.get(at);
    let paid_by = if let Seen::Paid(at, spent) = seen {
        row_at(at).map(|row| {
            money(spent, currency).map(|amount| PaidBy {
                transaction: row.transaction,
                posted_on: row.posted_on,
                amount,
            })
        })
    } else {
        None
    }
    .transpose()?;
    let amount = money(bill.amount, currency)?;
    adjustment.payments.push(OccurrencePayment {
        on: bill.on,
        amount,
        paid_by,
    });
    if matches!(seen, Seen::Paid(..)) || bill.on >= input.as_of {
        return Ok(());
    }

    adjustment.unpaid += bill.amount;
    outcome.spending_sensitive_currencies.insert(currency);
    if bill.on < input.as_of.first_of_month() {
        outcome.bill_cautions.push(Caution::BillUnpaidLastMonth {
            item: item.source,
            on: bill.on,
            amount,
        });
    } else {
        *outcome.not_seen.entry(bill.key).or_default() += bill.amount;
    }
    if let Seen::Unclear(at, spent) = seen
        && let Some(row) = row_at(at)
    {
        outcome.bill_cautions.push(Caution::BillPaymentUnclear {
            item: item.source,
            on: bill.on,
            amount,
            transaction: row.transaction,
            transaction_amount: money(spent, currency)?,
        });
    }
    Ok(())
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
