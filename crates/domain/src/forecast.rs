//! The cash forecast (spec §6.8, §8.9): what is free once the window's planned
//! spending, goals and the emergency fund are covered. Pure and exact: amounts add
//! up in `i128`, and any total that does not fit in `i64` refuses the answer.

use std::collections::BTreeSet;

use jiff::Span;
use jiff::civil::Date;

use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::plan::{EmergencyFund, EmergencyFundTarget, PlanSchedule, PlanTier};
use book::{Book, Bucket, Window, buckets};
use cautions::cautions;
use lines::{account_line, dated_line, goal_line, not_counted};

mod bill_matching;
mod book;
mod cautions;
mod free_money;
mod item_input;
mod lines;
mod reduction;
mod settled;
mod settled_budgets;
mod types;

pub use bill_matching::{
    BILL_AMOUNT_TOLERANCE_PERCENT, BILL_EARLY_PAYMENT_DAYS, BILL_LOOK_BACK_MONTHS,
    REFUND_UNDOES_FROM_PERCENT, REFUND_UNDOES_TO_PERCENT, bills_looked_at_from, payments_read_from,
};
pub use free_money::{FreeMoney, FreeMoneyCurrency, MUST_PAY_DAYS, StillNeed, compute_free_money};
pub use lines::{CountingFacts, left_out_of_forecast};
pub use types::{
    AccountInput, AccountLine, BalanceInput, Blocker, CategorySpent, Caution, CurrencyTotals,
    FeedAlert, FeedProblem, FeedProblemReason, FeedStatus, Forecast, ForecastError, ForecastInput,
    ForecastStatus, GoalLine, HintEffect, ItemInput, ItemLine, LineSource, LooseEnds, MonthBucket,
    MonthCurrency, MonthItem, NewerSpending, NotCountedReason, OccurrencePayment, PaidBy,
    RuleFiledTransfers, SettledBudget, SettledLine, SpentTransaction, UnboundAccount,
};

/// Computes the forecast for `input`, which the caller has validated.
///
/// # Errors
/// Returns [`ForecastError::OutOfRange`] when a total does not fit in `i64`, or
/// `as_of` is before year 0.
pub fn compute_forecast(input: &ForecastInput) -> Result<Forecast, ForecastError> {
    let end_exclusive = months_after(input.as_of, input.horizon_months);
    let income_end_exclusive = same_day_after(input.as_of, input.horizon_months);
    let buckets = buckets(input.as_of, end_exclusive)?;
    let counted: BTreeSet<AccountId> = input
        .accounts
        .iter()
        .filter(|account| not_counted(account).is_none())
        .map(|account| account.id)
        .collect();
    let window = Window {
        as_of: input.as_of,
        end_exclusive,
        income_end_exclusive,
        buckets: &buckets,
        counted: &counted,
    };
    let reduction = reduction::reduce(input, end_exclusive)?;

    let mut book = Book::new(buckets.len());
    let accounts: Vec<AccountLine> = input
        .accounts
        .iter()
        .map(|account| account_line(account, &mut book))
        .collect();
    let mut items = Vec::new();
    let mut month_counts = vec![0; buckets.len()];
    for (item, adjustment) in input.items.iter().zip(&reduction.by_item) {
        let line = match item.schedule {
            PlanSchedule::Goal {
                completed: true, ..
            } => None,
            PlanSchedule::Goal {
                start_on, due_on, ..
            } => goal_line(item, start_on, due_on, &window, &mut book)?,
            PlanSchedule::Recurring { .. } | PlanSchedule::OneOff { .. } => dated_line(
                item,
                adjustment,
                input.include_income,
                &window,
                &mut month_counts,
                &mut book,
            )?,
        };
        items.extend(line);
    }
    let fund_cautions = add_emergency_funds(input, &mut book)?;

    let blockers = blockers(input, &accounts, &book);
    let no_surplus = blockers
        .iter()
        .any(|blocker| !matches!(blocker, Blocker::MixedCurrencies { .. }));
    let mut cautions = cautions(input, &items, &reduction, fund_cautions, &counted)?;
    let (totals, months) = settle(book, &buckets, input, no_surplus, &mut cautions)?;

    let surplus = match (blockers.is_empty(), totals.as_slice()) {
        (true, [only]) => only.surplus,
        _ => None,
    };
    let status = if !blockers.is_empty() {
        ForecastStatus::Unavailable
    } else if cautions.is_empty() {
        ForecastStatus::Firm
    } else {
        ForecastStatus::Provisional
    };
    Ok(Forecast {
        as_of: input.as_of,
        end_exclusive,
        horizon_months: input.horizon_months,
        include_income: input.include_income,
        emergency_funds: totals
            .iter()
            .filter_map(|row| {
                input
                    .emergency_funds
                    .get(&row.currency)
                    .map(|&target| EmergencyFund {
                        currency: row.currency,
                        target,
                    })
            })
            .collect(),
        stale_after_days: input.stale_after_days,
        status,
        surplus,
        totals,
        accounts,
        items,
        months,
        blockers,
        cautions,
        rule_filed_transfers: reported_rule_filed_transfers(input),
        settled: settled::settled_lines(input, end_exclusive)?,
        settled_budgets: settled_budgets::settled_budgets(input, &reduction)?,
    })
}

/// The currencies with a transfer a rule filed with no link, sorted by currency.
fn reported_rule_filed_transfers(input: &ForecastInput) -> Vec<RuleFiledTransfers> {
    let mut rows: Vec<RuleFiledTransfers> = input
        .rule_filed_transfers
        .iter()
        .filter(|row| row.count > 0)
        .copied()
        .collect();
    rows.sort_by_key(|row| row.currency);
    rows
}

/// `date` plus `months`, clamped to the month's end, and to the calendar's.
fn same_day_after(date: Date, months: u8) -> Date {
    date.checked_add(Span::new().months(months))
        .unwrap_or(Date::MAX)
}

/// The end, exclusive, of the `months` from `date` for what must be paid: the same
/// day that many months on. A month too short to hold that day clamps to its last
/// day, which then stays inside the window, so a bill on it is not lost. A bill
/// anchored on a month's end may count once more for it, which is the safe direction
/// for an outflow only: income stops at [`same_day_after`].
fn months_after(date: Date, months: u8) -> Date {
    let same_day = same_day_after(date, months);
    if same_day.day() == date.day() {
        same_day
    } else {
        same_day.tomorrow().unwrap_or(Date::MAX)
    }
}

pub(crate) fn money(minor_units: i128, currency: Currency) -> Result<Money, ForecastError> {
    Ok(Money {
        minor_units: i64::try_from(minor_units).map_err(|_| ForecastError::OutOfRange)?,
        currency,
    })
}

/// How many `dates` there are: a daily item for 60 months fits many times over.
fn count(dates: &[Date]) -> Result<u32, ForecastError> {
    u32::try_from(dates.len()).map_err(|_| ForecastError::OutOfRange)
}

/// Sets each currency's emergency fund from its target, and what to say of any currency in use
/// with none, or with some months that come to 0. A target applies only to its own currency, and
/// only to one the book already has: it never puts a currency in use.
fn add_emergency_funds(
    input: &ForecastInput,
    book: &mut Book,
) -> Result<Vec<Caution>, ForecastError> {
    let mut said = Vec::new();
    for (&currency, tally) in &mut book.tallies {
        let fund = emergency_fund(input, currency)?;
        tally.emergency_fund = fund.minor_units;
        said.extend(fund.caution);
    }
    Ok(said)
}

/// One currency's emergency fund in minor units, and what makes it worth a word.
pub(super) struct Fund {
    /// 0 when no target is set.
    pub(super) minor_units: i128,
    /// `EmergencyFundNotSet` without a target, `EmergencyFundMonthsIsZero` for some months of
    /// outflows that come to 0.
    pub(super) caution: Option<Caution>,
}

/// `currency`'s emergency fund at `input.as_of`, which the forecast and free money both hold back.
/// With no target set, nothing is held back: the reason there was once no number without one was
/// not to overstate what can be invested, and the suggestion to invest is withheld instead
/// (spec §10.5).
pub(super) fn emergency_fund(
    input: &ForecastInput,
    currency: Currency,
) -> Result<Fund, ForecastError> {
    Ok(match input.emergency_funds.get(&currency) {
        None => Fund {
            minor_units: 0,
            caution: Some(Caution::EmergencyFundNotSet { currency }),
        },
        Some(&EmergencyFundTarget::Fixed { minor_units }) => Fund {
            minor_units: i128::from(minor_units),
            caution: None,
        },
        Some(&EmergencyFundTarget::MonthsOfOutflows { months }) => {
            let minor_units = months_of_outflows(input, currency, months)?;
            Fund {
                minor_units,
                caution: (minor_units == 0)
                    .then_some(Caution::EmergencyFundMonthsIsZero { currency, months }),
            }
        }
    })
}

/// What `currency`'s recurring must pay and essentials outflows come to over the `months`
/// from `as_of`: flexible spending can stop when money is short, so it isn't held for.
fn months_of_outflows(
    input: &ForecastInput,
    currency: Currency,
    months: u8,
) -> Result<i128, ForecastError> {
    let end_exclusive = months_after(input.as_of, months);
    let mut held = 0;
    for item in &input.items {
        let recurring = matches!(item.schedule, PlanSchedule::Recurring { .. });
        let held_for = matches!(item.tier, Some(PlanTier::MustPay | PlanTier::Essentials));
        if !recurring || !held_for || item.amount.currency != currency {
            continue;
        }
        let dates = item.occurrences(input.as_of, end_exclusive);
        held += i128::from(item.amount.minor_units) * i128::from(count(&dates)?);
    }
    Ok(held)
}

fn blockers(input: &ForecastInput, accounts: &[AccountLine], book: &Book) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    if book.tallies.len() > 1 {
        blockers.push(Blocker::MixedCurrencies {
            currencies: book.tallies.keys().copied().collect(),
        });
    }
    // A closed card's debt counts, but it is no cash to forecast from.
    let open_account_counts = input
        .accounts
        .iter()
        .any(|account| !account.closed && not_counted(account).is_none());
    if !open_account_counts {
        blockers.push(Blocker::NoCountedAccounts);
    }
    for line in accounts {
        if line.counted && line.balance.is_none() {
            // Every line comes from an input account, so the lookup always finds one.
            let currency = input
                .accounts
                .iter()
                .find(|account| account.id == line.account)
                .map(|account| account.currency);
            if let Some(currency) = currency {
                blockers.push(Blocker::BalanceMissing {
                    account: line.account,
                    name: line.name.clone(),
                    currency,
                });
            }
        }
    }
    blockers
}

/// Turns the running amounts into checked totals and month lines. A blocker other
/// than mixed currencies leaves every row without a surplus.
fn settle(
    book: Book,
    buckets: &[Bucket],
    input: &ForecastInput,
    no_surplus: bool,
    cautions: &mut Vec<Caution>,
) -> Result<(Vec<CurrencyTotals>, Vec<MonthBucket>), ForecastError> {
    let mut months: Vec<MonthBucket> = buckets
        .iter()
        .map(|bucket| MonthBucket {
            month: bucket.month,
            partial: bucket.partial,
            lines: Vec::new(),
        })
        .collect();
    let mut totals = Vec::new();
    for (currency, tally) in book.tallies {
        let inflows_counted = if input.include_income {
            tally.inflows
        } else {
            0
        };
        let needed = tally.outflows + tally.goals + tally.emergency_fund - inflows_counted;
        totals.push(CurrencyTotals {
            currency,
            cash_available: money(tally.cash, currency)?,
            planned_outflows: money(tally.outflows, currency)?,
            goals: money(tally.goals, currency)?,
            emergency_fund: money(tally.emergency_fund, currency)?,
            planned_inflows: money(inflows_counted, currency)?,
            excluded_inflows: money(tally.inflows - inflows_counted, currency)?,
            cash_needed: money(needed, currency)?,
            surplus: if no_surplus {
                None
            } else {
                Some(money(tally.cash - needed, currency)?)
            },
        });

        let has_target = input.emergency_funds.contains_key(&currency);
        let mut projected = tally.cash;
        let mut dipped = false;
        for (bucket, month) in months.iter_mut().zip(tally.months) {
            projected -= month.outflows;
            if input.include_income {
                projected += month.inflows;
            }
            let projected_cash = money(projected, currency)?;
            if input.include_income && has_target && !dipped && projected < tally.emergency_fund {
                dipped = true;
                cautions.push(Caution::CashDipsBelowEmergencyFund {
                    month: bucket.month,
                    currency,
                    projected_cash,
                });
            }
            bucket.lines.push(MonthCurrency {
                currency,
                outflows: money(month.outflows, currency)?,
                inflows: money(month.inflows, currency)?,
                goal_accrual: money(month.goal_accrual, currency)?,
                projected_cash,
                items: month.items,
            });
        }
    }
    Ok((totals, months))
}

#[cfg(test)]
mod tests;
