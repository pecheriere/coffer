//! How each account, planned item and goal enters the forecast: which accounts count as cash,
//! and the line each item gets once its dates in the window are added to the book.

use jiff::civil::Date;

use super::book::{Book, Window};
use super::reduction::{self, ItemAdjustment};
use super::settled;
use super::{
    AccountInput, AccountLine, ForecastError, GoalLine, ItemInput, ItemLine, NotCountedReason,
    count, money,
};
use crate::accounts::{BelongsTo, LiquidityClass};
use crate::money::Money;
use crate::plan::{PlanDirection, goal_need};

/// What the forecast reads of an account to decide whether its balance counts as cash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountingFacts {
    pub liquidity: LiquidityClass,
    pub belongs_to: BelongsTo,
    pub include_in_forecast: bool,
    pub closed: bool,
    /// The balance it is read at, in minor units; `None` when it has none.
    pub balance_minor: Option<i64>,
}

/// Why the forecast leaves an account out, or `None` when its balance counts: the one rule,
/// which the forecast and a page saying what it does with an account both call.
///
/// Money that isn't the household's never counts, and that is the reason whatever else
/// holds, since a flag can't change it (spec §8.11 B4, B8). A closed card's debt does not
/// close with it, so one that still owes counts, unless the household has switched it out.
#[must_use]
pub fn left_out_of_forecast(account: CountingFacts) -> Option<NotCountedReason> {
    let spendable = matches!(
        account.liquidity,
        LiquidityClass::Cash | LiquidityClass::ShortTermLiability
    );
    let owing = account.liquidity == LiquidityClass::ShortTermLiability
        && account.balance_minor.is_some_and(|minor| minor < 0);
    if account.belongs_to != BelongsTo::Household {
        Some(NotCountedReason::NotHousehold)
    } else if account.closed && owing && account.include_in_forecast {
        None
    } else if account.closed {
        Some(NotCountedReason::Closed)
    } else if !spendable {
        Some(NotCountedReason::NotCash)
    } else if !account.include_in_forecast {
        Some(NotCountedReason::ExcludedByFlag)
    } else {
        None
    }
}

/// [`left_out_of_forecast`] for an account as the forecast holds it.
pub(super) fn not_counted(account: &AccountInput) -> Option<NotCountedReason> {
    left_out_of_forecast(CountingFacts {
        liquidity: account.liquidity,
        belongs_to: account.belongs_to,
        include_in_forecast: account.include_in_forecast,
        closed: account.closed,
        balance_minor: account.balance.map(|balance| balance.minor_units),
    })
}

/// Whether the number reads this account: its balance is summed, or spending linked to a goal
/// on it lowers a goal's need. A feed that stalls on any other account changes nothing.
pub(super) fn read_by_number(account: &AccountInput) -> bool {
    not_counted(account).is_none() || account.goal_spending
}

pub(super) fn account_line(account: &AccountInput, book: &mut Book) -> AccountLine {
    let reason = not_counted(account);
    if reason.is_none() {
        let tally = book.tally(account.currency);
        if let Some(balance) = account.balance {
            tally.cash += i128::from(balance.minor_units);
        }
    }
    AccountLine {
        account: account.id,
        name: account.name.clone(),
        liquidity: account.liquidity,
        balance: account.balance.map(|balance| Money {
            minor_units: balance.minor_units,
            currency: account.currency,
        }),
        balance_on: account.balance.map(|balance| balance.on),
        counted: reason.is_none(),
        not_counted: reason,
    }
}

/// A recurring or one-off item's line, adding it to its currency's tally.
/// `month_counts` is a buffer, so a daily item costs no allocation per date.
pub(super) fn dated_line(
    item: &ItemInput,
    adjustment: &ItemAdjustment,
    include_income: bool,
    window: &Window<'_>,
    month_counts: &mut [i128],
    book: &mut Book,
) -> Result<Option<ItemLine>, ForecastError> {
    let outflow = item.direction == PlanDirection::Outflow;
    let end_exclusive = if outflow {
        window.end_exclusive
    } else {
        window.income_end_exclusive
    };
    let mut dates = item
        .schedule
        .occurrences(reduction::counts_from(item, window.as_of), end_exclusive);
    dates.retain(|&on| !settled::is_settled(item, on));
    let amount = i128::from(item.amount.minor_units);
    let currency = item.amount.currency;
    let occurrences = count(&dates)?;
    let gross = amount * i128::from(occurrences);
    let ItemAdjustment {
        reduced_by,
        unpaid,
        ref payments,
    } = *adjustment;
    // Paid and past, so listed for what paid it, and nowhere else: not even its currency is in use.
    let only_shown = gross == 0 && unpaid == 0;
    if only_shown && payments.is_empty() {
        return Ok(None);
    }
    // Listed, and nowhere else: not even its currency is in use.
    let uncounted_inflow = !outflow
        && item
            .account_hint
            .is_some_and(|hint| !window.counted.contains(&hint));

    if !uncounted_inflow && !only_shown {
        month_counts.fill(0);
        let mut bucket = 0;
        for &on in &dates {
            while window
                .buckets
                .get(bucket)
                .is_some_and(|found| found.end_exclusive <= on)
            {
                bucket += 1;
            }
            if let Some(count) = month_counts.get_mut(bucket) {
                *count += 1;
            }
        }

        let tally = book.tally(currency);
        if outflow {
            tally.outflows += gross - reduced_by + unpaid;
        } else {
            tally.inflows += gross;
        }
        for (index, (count, month)) in month_counts.iter().zip(&mut tally.months).enumerate() {
            // The reduction only touches occurrences in the first bucket, where a budget's
            // whole month lands, and a gap is still to be paid from there.
            let adjusted = if index == 0 { unpaid - reduced_by } else { 0 };
            let minor_units = count * amount + adjusted;
            if outflow {
                month.outflows += minor_units;
            } else {
                month.inflows += minor_units;
            }
            month.itemize(item.source, minor_units, currency)?;
        }
    }

    let counted = if outflow {
        gross - reduced_by + unpaid
    } else if include_income && !uncounted_inflow {
        gross
    } else {
        0
    };
    Ok(Some(ItemLine {
        source: item.source,
        name: item.name.clone(),
        item_type: item.schedule.item_type(),
        direction: item.direction,
        category: item.category,
        account_hint: item.account_hint,
        occurrences,
        first_on: dates.first().copied(),
        last_on: dates.last().copied(),
        gross: money(gross, currency)?,
        reduced_by: money(reduced_by, currency)?,
        unpaid: money(unpaid, currency)?,
        counted: money(counted, currency)?,
        goal: None,
        payments: payments.clone(),
    }))
}

/// A goal's line, adding its need to its currency's tally. The need at a date
/// past `due_on` is all that remains of the target, so one formula covers goals
/// due in the window, overdue ones and those due later, and the months telescope
/// to the total: each adds the need at its end less the need at the last one's.
pub(super) fn goal_line(
    item: &ItemInput,
    start_on: Date,
    due_on: Date,
    window: &Window<'_>,
    book: &mut Book,
) -> Result<Option<ItemLine>, ForecastError> {
    let target = item.amount.minor_units;
    let currency = item.amount.currency;
    let need = |at| {
        i128::from(goal_need(
            target,
            item.goal_spent_minor,
            start_on,
            due_on,
            at,
        ))
    };
    let gross = need(window.end_exclusive);
    if gross == 0 {
        return Ok(None);
    }

    let tally = book.tally(currency);
    tally.goals += gross;
    let mut so_far = 0;
    for (bucket, month) in window.buckets.iter().zip(&mut tally.months) {
        let by_then = need(bucket.end_exclusive);
        let added = by_then - so_far;
        month.goal_accrual += added;
        month.itemize(item.source, added, currency)?;
        so_far = by_then;
    }

    let counted = money(gross, currency)?;
    Ok(Some(ItemLine {
        source: item.source,
        name: item.name.clone(),
        item_type: item.schedule.item_type(),
        direction: item.direction,
        category: item.category,
        account_hint: item.account_hint,
        occurrences: 0,
        first_on: None,
        last_on: None,
        gross: counted,
        reduced_by: money(0, currency)?,
        unpaid: money(0, currency)?,
        counted,
        goal: Some(GoalLine {
            target: item.amount,
            spent: Money {
                minor_units: item.goal_spent_minor.max(0),
                currency,
            },
            due_on,
            due_in_horizon: (window.as_of..window.end_exclusive).contains(&due_on),
        }),
        payments: Vec::new(),
    }))
}
