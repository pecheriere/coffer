//! Which transaction paid which dated bill (spec §8.9 R9b). A recurring bill or a one-off is a
//! specific payment, so it is paid by a specific transaction, never by a category's total: that
//! is what stops money spent on a budget hiding an unpaid bill, and an October bill posting in
//! November paying November's.
//!
//! Deterministic: transactions go oldest first, and each pays the oldest occurrence it fits, in
//! its category and currency, by amount. It pays one at most. Whatever it can't be told apart
//! from a budget's spending pays nothing, and stays in the budget's pool, so the bill still
//! counts: the safe direction, and the caller says so with a caution.
//!
//! A payment may pay an occurrence not yet due, up to [`BILL_EARLY_PAYMENT_DAYS`] after `as_of`
//! (spec §10.8), so a bill paid a few days early needs nothing from the day it was paid. An older
//! unpaid occurrence it fits still comes first.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::reduction::{PoolKey, is_stored_budget, pool_key};
use super::types::{ForecastInput, SpentTransaction};
use crate::plan::{CalendarMonth, SettlementKind};

/// How many calendar months before `as_of`'s a bill can still be unpaid from.
pub const BILL_LOOK_BACK_MONTHS: i8 = 1;

/// How far, as a percentage of the bill's amount either way, a transaction may differ from it
/// and still pay it: a utility varies, and a transaction further off says nothing.
pub const BILL_AMOUNT_TOLERANCE_PERCENT: i128 = 10;

/// How many days before its date a transaction may pay a bill. Direct debits move around a
/// weekend, but last month's second payment isn't next month's first.
pub const BILL_EARLY_PAYMENT_DAYS: i64 = 7;

/// A refund counts against a charge from this percentage of it: 250 back on a rent of 2,000 is
/// something else coming back, and 1,000 back says the rent wasn't paid.
pub const REFUND_UNDOES_FROM_PERCENT: i128 = 25;

/// And up to this one: 1,500 back isn't about a charge of 30, however new that is.
pub const REFUND_UNDOES_TO_PERCENT: i128 = 110;

/// The first day whose bills a forecast from `as_of` still looks at, and whose transactions it
/// reads to pay them.
#[must_use]
pub fn bills_looked_at_from(as_of: Date) -> Date {
    as_of
        .first_of_month()
        .checked_sub(Span::new().months(BILL_LOOK_BACK_MONTHS))
        .unwrap_or(Date::MIN)
}

/// The first day whose transactions a forecast from `as_of` reads: a payment up to
/// [`BILL_EARLY_PAYMENT_DAYS`] before the first bill it looks at may be that bill's.
#[must_use]
pub fn payments_read_from(as_of: Date) -> Date {
    bills_looked_at_from(as_of)
        .checked_sub(Span::new().days(BILL_EARLY_PAYMENT_DAYS))
        .unwrap_or(Date::MIN)
}

/// One occurrence of a stored recurring bill or one-off that the look-back reaches, one not settled
/// in the [`BILL_EARLY_PAYMENT_DAYS`] after `as_of`, which a payment already made may have paid, or
/// one settled as paid later in `as_of`'s month or in that week.
#[derive(Clone, Copy)]
pub(super) struct Bill {
    pub(super) on: Date,
    /// The item's position in the input.
    pub(super) index: usize,
    pub(super) key: PoolKey,
    pub(super) amount: i128,
    /// What a person said of its month. A settled occurrence takes no part in paying.
    pub(super) settled: Option<SettlementKind>,
    /// Dated before the first transaction anyone can see, so nothing can say it was paid or not.
    pub(super) before_history: bool,
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
    /// What occurrences settled as paid in `as_of`'s month take from their pool, where no
    /// transaction fits them: the money was spent somehow, and may be in the pool.
    pub(super) settled_charges: BTreeMap<PoolKey, i128>,
    /// The refunds that undid no charge.
    pub(super) uncounted_refunds: Vec<usize>,
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
    fn fits(self, bill: &Bill, input: &ForecastInput) -> bool {
        let earliest = bill
            .on
            .checked_sub(Span::new().days(BILL_EARLY_PAYMENT_DAYS))
            .unwrap_or(Date::MIN);
        key_of(self.row) == bill.key
            && earliest <= self.row.posted_on
            && (close_enough(self.amount, bill.amount)
                || input.items.get(bill.index).is_some_and(|item| {
                    item.also_matches_on(bill.on)
                        .any(|held| close_enough(self.amount, held))
                }))
            && (self.row.posted_on >= bills_looked_at_from(input.as_of)
                || self.is_closest_to(bill, input))
    }

    /// Whether this payment, read from before the look-back, is nearer `bill` than the occurrence
    /// before it. If not, it paid that one: taking it here would shift a weekly bill's payments
    /// along by one and hide a missed week.
    fn is_closest_to(self, bill: &Bill, input: &ForecastInput) -> bool {
        let days_to = |from: Date, to: Date| from.until(to).map_or(0, |span| span.get_days().abs());
        let window = bill
            .on
            .checked_sub(Span::new().days(2 * BILL_EARLY_PAYMENT_DAYS))
            .unwrap_or(Date::MIN);
        let previous = input
            .items
            .get(bill.index)
            .and_then(|item| item.neighbours(window, bill.on).last().copied());
        previous.is_none_or(|previous| {
            days_to(self.row.posted_on, bill.on) < days_to(previous, self.row.posted_on)
        })
    }
}

/// Whether a refund of `refunded` counts against a charge of `charge`.
fn undoes(refunded: i128, charge: i128) -> bool {
    (charge * REFUND_UNDOES_FROM_PERCENT..=charge * REFUND_UNDOES_TO_PERCENT)
        .contains(&(refunded * 100))
}

/// What each outflow can still pay, and the refunds that left all of them standing. A refund undoes
/// the newest charge it is the right size for, in its category and currency and on or before its
/// own day, and that charge then pays nothing, the safe direction. Any other refund only lowers what
/// its category spent, which a budget's pool reads.
fn after_refunds(rows: &[SpentTransaction], order: &[usize]) -> (Vec<i128>, Vec<usize>) {
    let mut left: Vec<i128> = rows
        .iter()
        .map(|row| i128::from(row.spent_minor).max(0))
        .collect();
    let mut uncounted = Vec::new();
    for &refund_at in order {
        let Some(refund) = rows.get(refund_at).filter(|row| row.spent_minor < 0) else {
            continue;
        };
        let refunded = -i128::from(refund.spent_minor);
        let undone = order.iter().rev().copied().find(|&at| {
            rows.get(at)
                .zip(left.get(at))
                .is_some_and(|(row, &amount)| {
                    amount > 0
                        && key_of(row) == key_of(refund)
                        && row.posted_on <= refund.posted_on
                        && undoes(refunded, amount)
                })
        });
        match undone.and_then(|at| left.get_mut(at)) {
            Some(amount) => *amount = 0,
            None => uncounted.push(refund_at),
        }
    }
    (left, uncounted)
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
                    let held = item.occurrences_in(month).len();
                    i128::from(item.amount.minor_units)
                        .saturating_mul(i128::try_from(held).unwrap_or(i128::MAX))
                })
                .sum()
        })
    }

    /// Whether `payment`, which fits `bill`, can be told from a budget's spending: it is the
    /// bill's exact amount (or one it held during the month), or more than the category's budgets plan for its whole month.
    fn tells_apart(&mut self, payment: Payment<'_>, bill: &Bill) -> bool {
        payment.amount == bill.amount
            || self.input.items.get(bill.index).is_some_and(|item| {
                item.also_matches_on(bill.on)
                    .any(|held| held == payment.amount)
            })
            || CalendarMonth::of(payment.row.posted_on)
                .is_ok_and(|month| payment.amount > self.of(bill.key, month))
    }
}

/// Matches `bills`, sorted by date and then input order, against the transactions. A possible
/// duplicate pays nothing, since it may be the same payment again, and neither does a charge a
/// refund undid.
pub(super) fn match_bills(bills: &[Bill], input: &ForecastInput) -> Matching {
    let rows = &input.spent_transactions;
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&at| rows.get(at).map(|row| (row.posted_on, row.transaction)));
    let (left, uncounted_refunds) = after_refunds(rows, &order);
    let payments: Vec<Payment<'_>> = order
        .into_iter()
        .filter_map(|at| {
            let row = rows.get(at).filter(|row| !row.possible_duplicate)?;
            let amount = left.get(at).copied().filter(|&amount| amount > 0)?;
            Some(Payment { at, row, amount })
        })
        .collect();

    // A payment already made may be for a bill up to a week ahead, as `Payment::fits` allows.
    let payable_until = input
        .as_of
        .checked_add(Span::new().days(BILL_EARLY_PAYMENT_DAYS))
        .unwrap_or(Date::MAX);
    let mut plans = BudgetPlans {
        input,
        known: BTreeMap::new(),
    };
    let mut matching = Matching {
        seen: vec![Seen::Nothing; bills.len()],
        taken: BTreeMap::new(),
        settled_charges: BTreeMap::new(),
        uncounted_refunds,
    };
    for &payment in &payments {
        let oldest = bills
            .iter()
            .enumerate()
            .filter(|&(position, bill)| {
                matching.seen.get(position) == Some(&Seen::Nothing)
                    && bill.settled.is_none()
                    && !bill.before_history
                    && bill.on <= payable_until
                    && payment.fits(bill, input)
                    && plans.tells_apart(payment, bill)
            })
            .min_by_key(|&(position, bill)| {
                (bill.on, (payment.amount - bill.amount).abs(), position)
            });
        if let Some((position, _)) = oldest
            && let Some(found) = matching.seen.get_mut(position)
        {
            *found = Seen::Paid(payment.at, payment.amount);
            matching.taken.insert(payment.at, payment.amount);
        }
    }
    point_at_the_rest(bills, input, &payments, &mut matching);
    matching
}

/// What the transactions left over say of the bills they didn't pay.
fn point_at_the_rest(
    bills: &[Bill],
    input: &ForecastInput,
    payments: &[Payment<'_>],
    matching: &mut Matching,
) {
    let month_start = input.as_of.first_of_month();
    let mut pointed_at = BTreeSet::new();
    // What last month's category spent that paid nothing: a settled bill may be why.
    let mut last_month_free: BTreeMap<PoolKey, i128> = BTreeMap::new();
    for payment in payments.iter().filter(|payment| {
        payment.row.posted_on < month_start && !matching.taken.contains_key(&payment.at)
    }) {
        *last_month_free.entry(key_of(payment.row)).or_default() += payment.amount;
    }
    for (position, bill) in bills.iter().enumerate() {
        let open = match bill.settled {
            // Dated before the history it owes nothing anyway, so settling it saves nothing, and
            // it takes nothing either.
            Some(SettlementKind::Paid) => !bill.before_history,
            Some(SettlementKind::Skipped) => false,
            // Dated `as_of`, it counts in full anyway, so there is nothing to say.
            None => bill.on < input.as_of && !bill.before_history,
        };
        if !open || matching.seen.get(position) != Some(&Seen::Nothing) {
            continue;
        }
        let found = payments.iter().find(|payment| {
            !matching.taken.contains_key(&payment.at)
                && !pointed_at.contains(&payment.at)
                && payment.fits(bill, input)
        });
        match (found, bill.settled) {
            // A person said it was paid, and this is likely how. Left in the pool, the same money
            // would lower a budget as well, what is over the bill included, as it would for a bill
            // it matched.
            (Some(payment), Some(_)) => {
                matching.taken.insert(payment.at, payment.amount);
            }
            // Paid some way nothing fits, such as in two parts: any of the category's money may
            // be how, so that much of it is no budget's. For last month's bill, that is what
            // last month's money didn't cover.
            (None, Some(_)) => {
                let mut charge = bill.amount;
                if bill.on < month_start {
                    let free = last_month_free.entry(bill.key).or_default();
                    let covered = charge.min(*free);
                    *free -= covered;
                    charge -= covered;
                }
                *matching.settled_charges.entry(bill.key).or_default() += charge;
            }
            // It stays with the budget, but no more than the bill: were it the payment, what is
            // over isn't the budget's either, and the need would read below the truth.
            (Some(payment), None) => {
                if let Some(seen) = matching.seen.get_mut(position) {
                    *seen = Seen::Unclear(payment.at, payment.amount);
                }
                pointed_at.insert(payment.at);
                if payment.amount > bill.amount {
                    matching
                        .taken
                        .insert(payment.at, payment.amount - bill.amount);
                }
            }
            (None, None) => {}
        }
    }
}
