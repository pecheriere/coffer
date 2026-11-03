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
//! A settled occurrence (R9a) is out of all of this: it owes no gap and pays no transaction away
//! from another bill. One settled as paid does keep what it was paid with off a budget, but never
//! so far that settling raises the need.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::bill_matching::{Bill, Matching, Seen, bills_looked_at_from, match_bills};
use super::money;
use super::settled::settlement_of;
use super::types::{
    Caution, ForecastError, ForecastInput, ItemInput, LineSource, OccurrencePayment, PaidBy,
};
use crate::ids::CategoryId;
use crate::money::Currency;
use crate::plan::{PlanDirection, PlanSchedule, PlanSpending, SettlementKind};

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

#[derive(Default)]
pub(super) struct NotSeen {
    pub(super) amount: i128,
    pub(super) items: Vec<LineSource>,
}

pub(super) struct Reduction {
    /// One per item, in input order.
    pub(super) by_item: Vec<ItemAdjustment>,
    /// The gaps dated this month, per category and currency, and the items they belong to.
    pub(super) not_seen: BTreeMap<PoolKey, NotSeen>,
    /// What to say of a single occurrence: unpaid from last month, or unclear.
    pub(super) bill_cautions: Vec<Caution>,
    /// The currencies with a bill from last month unpaid.
    pub(super) last_month_unpaid_currencies: BTreeSet<Currency>,
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
            // has not passed, so it owes no gap. One settled as paid later this month was paid,
            // so it has a claim on what was spent too.
            let bill = |on| Bill {
                on,
                index,
                key,
                amount,
                settled: settlement_of(item, on),
                // Last month's only: this month's is what a balance-only household still has to
                // answer for, with a caution and a way to settle it.
                before_history: on < month_start
                    && input.earliest_transaction.is_none_or(|first| on < first),
            };
            let dates = item.schedule.occurrences(looked_at_from, through_as_of);
            bills.extend(dates.into_iter().map(bill));
            let later = item.schedule.occurrences(through_as_of, reducible_until);
            bills.extend(
                later
                    .into_iter()
                    .map(bill)
                    .filter(|one| one.settled == Some(SettlementKind::Paid)),
            );
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

    let pools = pools_settled(&pools, &bills, &matching, &month_budgets, input);

    let Outcome {
        not_seen,
        mut bill_cautions,
        last_month_unpaid_currencies,
        missing_history,
        mut spending_sensitive_currencies,
    } = outcome;
    bill_cautions.extend(refund_cautions(input, &bills, &matching)?);
    for (currency, (occurrences, amount)) in missing_history {
        bill_cautions.push(Caution::BillHistoryMissing {
            starts_on: input.earliest_transaction,
            currency,
            occurrences,
            amount: money(amount, currency)?,
        });
    }
    spending_sensitive_currencies.extend(month_budgets.iter().map(|one| one.key.1));
    reduce_budgets(month_budgets, pools, &mut by_item);

    Ok(Reduction {
        by_item,
        not_seen,
        bill_cautions,
        last_month_unpaid_currencies,
        spending_sensitive_currencies,
    })
}

/// What each of `spent`'s pools holds once `matching` has taken what paid a bill.
fn pools_after(
    spent: &BTreeMap<PoolKey, i128>,
    matching: &Matching,
    input: &ForecastInput,
) -> BTreeMap<PoolKey, i128> {
    let month_start = input.as_of.first_of_month();
    let mut pools = spent.clone();
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
    for (key, &charged) in &matching.settled_charges {
        if let Some(pool) = pools.get_mut(key) {
            *pool -= charged;
        }
    }
    pools
}

/// What `seen` leaves `bills` counting, per category and currency: an occurrence dated `as_of` or
/// later counts in full, and one before that unless it was paid. Nothing that is settled or dated
/// before the history counts.
fn bills_counted(bills: &[Bill], seen: &[Seen], as_of: Date) -> BTreeMap<PoolKey, i128> {
    let mut counted: BTreeMap<PoolKey, i128> = BTreeMap::new();
    for (bill, seen) in bills.iter().zip(seen) {
        let counts = bill.settled.is_none()
            && !bill.before_history
            && (bill.on >= as_of || !matches!(seen, Seen::Paid(..)));
        if counts {
            *counted.entry(bill.key).or_default() += bill.amount;
        }
    }
    counted
}

/// The pools `matching` leaves, none lower than what its budgets would use with nothing settled,
/// less what settling saved in its category. A budget's need rises by no more than that use falls,
/// so settling can't raise the need, however a settled bill sends a payment on: to today's bill,
/// which counts in full whatever pays it, or to a smaller one in tolerance.
fn pools_settled(
    spent: &BTreeMap<PoolKey, i128>,
    bills: &[Bill],
    matching: &Matching,
    month_budgets: &[Occurrence],
    input: &ForecastInput,
) -> BTreeMap<PoolKey, i128> {
    let mut pools = pools_after(spent, matching, input);
    if bills.iter().all(|bill| bill.settled.is_none()) {
        return pools;
    }
    let plain: Vec<Bill> = bills
        .iter()
        .map(|bill| Bill {
            settled: None,
            ..*bill
        })
        .collect();
    let unsettled = match_bills(&plain, input);
    let pools_unsettled = pools_after(spent, &unsettled, input);
    let counted_unsettled = bills_counted(&plain, &unsettled.seen, input.as_of);
    let counted = bills_counted(bills, &matching.seen, input.as_of);
    let mut planned: BTreeMap<PoolKey, i128> = BTreeMap::new();
    for budget in month_budgets {
        *planned.entry(budget.key).or_default() += budget.amount;
    }
    for (key, pool) in &mut pools {
        let before = pools_unsettled
            .get(key)
            .copied()
            .unwrap_or_default()
            .min(planned.get(key).copied().unwrap_or_default());
        let saved = counted_unsettled.get(key).copied().unwrap_or_default()
            - counted.get(key).copied().unwrap_or_default();
        *pool = (*pool).max(before - saved);
    }
    pools
}

/// What to say of refunds that undid no charge, beside a bill paid in their category.
fn refund_cautions(
    input: &ForecastInput,
    bills: &[Bill],
    matching: &Matching,
) -> Result<Vec<Caution>, ForecastError> {
    let paid_keys: BTreeSet<PoolKey> = bills
        .iter()
        .zip(&matching.seen)
        .filter(|(_, seen)| matches!(seen, Seen::Paid(..)))
        .map(|(bill, _)| bill.key)
        .collect();
    matching
        .uncounted_refunds
        .iter()
        .filter_map(|&at| input.spent_transactions.get(at))
        .filter(|row| paid_keys.contains(&(row.category, row.currency)))
        .map(|row| {
            Ok(Caution::RefundBesidePaidBill {
                category: row.category,
                transaction: row.transaction,
                amount: money(-i128::from(row.spent_minor), row.currency)?,
            })
        })
        .collect()
}

/// What the occurrences add up to, across items.
#[derive(Default)]
struct Outcome {
    not_seen: BTreeMap<PoolKey, NotSeen>,
    bill_cautions: Vec<Caution>,
    last_month_unpaid_currencies: BTreeSet<Currency>,
    /// How many occurrences before the first transaction, and how much, per currency.
    missing_history: BTreeMap<Currency, (u32, i128)>,
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
            Ok(PaidBy {
                transaction: row.transaction,
                posted_on: row.posted_on,
                amount: money(spent, currency)?,
                difference: money(spent - bill.amount, currency)?,
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
        before_history: bill.before_history,
    });
    if bill.before_history {
        let (occurrences, total) = outcome.missing_history.entry(currency).or_default();
        *occurrences = occurrences.saturating_add(1);
        *total += bill.amount;
        return Ok(());
    }
    if matches!(seen, Seen::Paid(..)) || bill.on >= input.as_of {
        return Ok(());
    }

    adjustment.unpaid += bill.amount;
    outcome.spending_sensitive_currencies.insert(currency);
    if bill.on < input.as_of.first_of_month() {
        outcome.last_month_unpaid_currencies.insert(currency);
        outcome.bill_cautions.push(Caution::BillUnpaidLastMonth {
            item: item.source,
            on: bill.on,
            amount,
        });
    } else {
        let gap = outcome.not_seen.entry(bill.key).or_default();
        gap.amount += bill.amount;
        if !gap.items.contains(&item.source) {
            gap.items.push(item.source);
        }
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
