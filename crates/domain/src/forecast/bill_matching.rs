//! Which transaction paid which dated bill (spec §8.9 R9b). A recurring bill or a one-off is a
//! specific payment, so it is paid by a specific transaction, never by a category's total: that
//! is what stops money spent on a budget hiding an unpaid bill, and an October bill posting in
//! November paying November's.
//!
//! Deterministic: transactions go oldest first, and each pays the oldest occurrence it fits, in
//! its category and currency, by amount. It pays one at most. Whatever it can't be told apart
//! from a budget's spending pays nothing, and stays in the budget's pool, so the bill still
//! counts: the safe direction, and the caller says so with a caution.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::reduction::{PoolKey, is_stored_budget, pool_key};
use super::types::{ForecastInput, SpentTransaction};
use crate::plan::{CalendarMonth, SettlementKind, occurrences_in};

/// How many calendar months before `as_of`'s a bill can still be unpaid from.
pub const BILL_LOOK_BACK_MONTHS: i8 = 1;

/// How far, as a percentage of the bill's amount either way, a transaction may differ from it
/// and still pay it: a utility varies, and a transaction further off says nothing.
pub const BILL_AMOUNT_TOLERANCE_PERCENT: i128 = 10;

/// How many days before its date a transaction may pay a bill. Direct debits move around a
/// weekend, but last month's second payment isn't next month's first.
pub const BILL_EARLY_PAYMENT_DAYS: i64 = 7;

/// The first day whose bills a forecast from `as_of` still looks at, and whose transactions it
/// reads to pay them.
#[must_use]
pub fn bills_looked_at_from(as_of: Date) -> Date {
    as_of
        .first_of_month()
        .checked_sub(Span::new().months(BILL_LOOK_BACK_MONTHS))
        .unwrap_or(Date::MIN)
}

/// One occurrence of a stored recurring bill or one-off that the look-back reaches.
pub(super) struct Bill {
    pub(super) on: Date,
    /// The item's position in the input.
    pub(super) index: usize,
    pub(super) key: PoolKey,
    pub(super) amount: i128,
    /// What a person said of its month. A settled occurrence takes no part in paying.
    pub(super) settled: Option<SettlementKind>,
}

/// What the transactions say of one occurrence. The numbers are positions in the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Seen {
    Nothing,
    /// This transaction paid it, with this much of it left after refunds.
    Paid(usize, i128),
    /// This transaction might have paid it, but might be a budget's spending.
    Unclear(usize, i128),
}

pub(super) struct Matching {
    /// One per bill, in the order given.
    pub(super) seen: Vec<Seen>,
    /// The transactions that come out of a budget's pool, and how much of each.
    pub(super) taken: BTreeMap<usize, i128>,
}

/// Whether `paid` is within the tolerance of `planned`, a positive amount.
fn close_enough(paid: i128, planned: i128) -> bool {
    (paid - planned).abs() * 100 <= planned * BILL_AMOUNT_TOLERANCE_PERCENT
}

fn key_of(row: &SpentTransaction) -> PoolKey {
    (row.category, row.currency)
}

/// An outflow that can pay a bill, as much of it as refunds left.
#[derive(Clone, Copy)]
struct Payment<'a> {
    at: usize,
    row: &'a SpentTransaction,
    amount: i128,
}

impl Payment<'_> {
    /// Whether it could pay `bill`, leaving its budget aside.
    fn fits(self, bill: &Bill) -> bool {
        let earliest = bill
            .on
            .checked_sub(Span::new().days(BILL_EARLY_PAYMENT_DAYS))
            .unwrap_or(Date::MIN);
        key_of(self.row) == bill.key
            && earliest <= self.row.posted_on
            && close_enough(self.amount, bill.amount)
    }
}

/// The outflows in date order, each less the refunds in its category and currency, which come
/// off the newest charge on or before their own day, then the one before it. What is left can be
/// too little to pay anything, which holds more, whichever charge the refund was really for.
fn after_refunds(rows: &[SpentTransaction], order: &[usize]) -> Vec<i128> {
    let mut left: Vec<i128> = rows
        .iter()
        .map(|row| i128::from(row.spent_minor).max(0))
        .collect();
    for &refund_at in order {
        let Some(refund) = rows.get(refund_at).filter(|row| row.spent_minor < 0) else {
            continue;
        };
        let mut to_take = -i128::from(refund.spent_minor);
        for &at in order.iter().rev() {
            if let (Some(row), Some(amount)) = (rows.get(at), left.get_mut(at))
                && key_of(row) == key_of(refund)
                && row.posted_on <= refund.posted_on
            {
                let taken = to_take.min(*amount);
                *amount -= taken;
                to_take -= taken;
            }
        }
    }
    left
}

/// What a category's budgets plan for a calendar month.
struct BudgetPlans<'a> {
    input: &'a ForecastInput,
    known: BTreeMap<(PoolKey, CalendarMonth), i128>,
}

impl BudgetPlans<'_> {
    fn of(&mut self, key: PoolKey, month: CalendarMonth) -> i128 {
        let input = self.input;
        *self.known.entry((key, month)).or_insert_with(|| {
            input
                .items
                .iter()
                .filter(|item| is_stored_budget(item) && pool_key(item) == Some(key))
                .map(|item| {
                    let held = occurrences_in(&item.schedule, month).len();
                    i128::from(item.amount.minor_units)
                        .saturating_mul(i128::try_from(held).unwrap_or(i128::MAX))
                })
                .sum()
        })
    }

    /// Whether `payment`, which fits `bill`, can be told from a budget's spending: it is the
    /// bill's exact amount, or more than the category's budgets plan for its whole month.
    fn tells_apart(&mut self, payment: Payment<'_>, bill: &Bill) -> bool {
        payment.amount == bill.amount
            || CalendarMonth::of(payment.row.posted_on)
                .is_ok_and(|month| payment.amount > self.of(bill.key, month))
    }
}

/// Matches `bills`, sorted by date and then input order, against the transactions. A possible
/// duplicate pays nothing, since it may be the same payment again, and neither does a charge
/// refunds emptied.
pub(super) fn match_bills(bills: &[Bill], input: &ForecastInput) -> Matching {
    let rows = &input.spent_transactions;
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&at| rows.get(at).map(|row| (row.posted_on, row.transaction)));
    let left = after_refunds(rows, &order);
    let payments: Vec<Payment<'_>> = order
        .into_iter()
        .filter_map(|at| {
            let row = rows.get(at).filter(|row| !row.possible_duplicate)?;
            let amount = left.get(at).copied().filter(|&amount| amount > 0)?;
            Some(Payment { at, row, amount })
        })
        .collect();

    let mut plans = BudgetPlans {
        input,
        known: BTreeMap::new(),
    };
    let mut seen = vec![Seen::Nothing; bills.len()];
    let mut taken = BTreeMap::new();
    for &payment in &payments {
        let oldest = bills
            .iter()
            .enumerate()
            .filter(|&(position, bill)| {
                seen.get(position) == Some(&Seen::Nothing)
                    && bill.settled.is_none()
                    && payment.fits(bill)
                    && plans.tells_apart(payment, bill)
            })
            .min_by_key(|&(position, bill)| {
                (bill.on, (payment.amount - bill.amount).abs(), position)
            });
        if let Some((position, _)) = oldest
            && let Some(found) = seen.get_mut(position)
        {
            *found = Seen::Paid(payment.at, payment.amount);
            taken.insert(payment.at, payment.amount);
        }
    }

    let mut pointed_at = BTreeSet::new();
    for (position, bill) in bills.iter().enumerate() {
        let open = match bill.settled {
            // A person said it was paid, and this is likely how: left in the pool, the same money
            // would lower a budget as well.
            Some(SettlementKind::Paid) => true,
            Some(SettlementKind::Skipped) => false,
            // Dated `as_of`, it counts in full anyway, so there is nothing to say.
            None => bill.on < input.as_of,
        };
        if !open || seen.get(position) != Some(&Seen::Nothing) {
            continue;
        }
        let Some(&payment) = payments.iter().find(|payment| {
            !taken.contains_key(&payment.at)
                && !pointed_at.contains(&payment.at)
                && payment.fits(bill)
        }) else {
            continue;
        };
        if bill.settled.is_some() {
            // No more than the bill, so settling it can't raise what is needed.
            taken.insert(payment.at, payment.amount.min(bill.amount));
        } else if let Some(found) = seen.get_mut(position) {
            *found = Seen::Unclear(payment.at, payment.amount);
            pointed_at.insert(payment.at);
        }
    }
    Matching { seen, taken }
}
