//! The cash forecast (spec §6.8, §8.9): what is free once the window's planned
//! spending, goals and the safety floor are covered. Pure and exact: amounts add
//! up in `i128`, and any total that does not fit in `i64` refuses the answer.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use crate::accounts::{BelongsTo, LiquidityClass};
use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::plan::{CalendarMonth, PlanDirection, PlanSchedule, SafetyFloor, goal_need};
use reduction::{ItemAdjustment, Reduction};

mod bill_matching;
mod reduction;
mod settled;
mod types;

pub use bill_matching::{
    BILL_AMOUNT_TOLERANCE_PERCENT, BILL_EARLY_PAYMENT_DAYS, BILL_LOOK_BACK_MONTHS,
    bills_looked_at_from, payments_read_from,
};
pub use types::{
    AccountInput, AccountLine, BalanceInput, Blocker, CategorySpent, Caution, CurrencyTotals,
    FeedAlert, FeedProblem, FeedProblemReason, FeedStatus, Forecast, ForecastError, ForecastInput,
    ForecastStatus, GoalLine, HintEffect, ItemInput, ItemLine, LineSource, LooseEnds, MonthBucket,
    MonthCurrency, MonthItem, NewerSpending, NotCountedReason, OccurrencePayment, PaidBy,
    SettledLine, SpentTransaction, UnboundAccount,
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
    add_floor(input, &mut book)?;

    let blockers = blockers(input, &accounts, &book);
    let no_surplus = blockers
        .iter()
        .any(|blocker| !matches!(blocker, Blocker::MixedCurrencies { .. }));
    let mut cautions = cautions(input, &items, &reduction, &book, &counted)?;
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
        floor: input.floor,
        stale_after_days: input.stale_after_days,
        status,
        surplus,
        totals,
        accounts,
        items,
        months,
        blockers,
        cautions,
        settled: settled::settled_lines(input, end_exclusive)?,
    })
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

/// The days a forecast looks at.
struct Window<'a> {
    as_of: Date,
    end_exclusive: Date,
    /// Where planned inflows stop: never past the same day `horizon_months` on, so
    /// the clamped day adds no income.
    income_end_exclusive: Date,
    buckets: &'a [Bucket],
    /// The accounts that count as cash: income expected anywhere else may never reach them.
    counted: &'a BTreeSet<AccountId>,
}

/// The part of the window in one calendar month.
struct Bucket {
    month: CalendarMonth,
    partial: bool,
    /// Where this bucket stops: the next month's first day or the window's end.
    end_exclusive: Date,
}

fn buckets(as_of: Date, end_exclusive: Date) -> Result<Vec<Bucket>, ForecastError> {
    let mut month = CalendarMonth::of(as_of).map_err(|_| ForecastError::OutOfRange)?;
    let mut start = as_of;
    let mut found = Vec::new();
    while start < end_exclusive {
        let next = month.next().first_day();
        // The calendar's last month is its own successor, and ends the window.
        let stop = if next > start {
            next.min(end_exclusive)
        } else {
            end_exclusive
        };
        found.push(Bucket {
            month,
            partial: start != month.first_day() || stop != next,
            end_exclusive: stop,
        });
        start = stop;
        month = month.next();
    }
    Ok(found)
}

/// One currency's running amounts. A row exists exactly for a currency in use.
struct Tally {
    cash: i128,
    outflows: i128,
    goals: i128,
    /// Every planned inflow in the window, counted or not, but for one expected in an account
    /// that isn't the household's.
    inflows: i128,
    floor: i128,
    months: Vec<MonthTally>,
}

#[derive(Default)]
struct MonthTally {
    outflows: i128,
    inflows: i128,
    goal_accrual: i128,
    items: Vec<MonthItem>,
}

impl MonthTally {
    fn itemize(
        &mut self,
        source: LineSource,
        minor_units: i128,
        currency: Currency,
    ) -> Result<(), ForecastError> {
        if minor_units != 0 {
            self.items.push(MonthItem {
                source,
                amount: money(minor_units, currency)?,
            });
        }
        Ok(())
    }
}

struct Book {
    bucket_count: usize,
    tallies: BTreeMap<Currency, Tally>,
}

impl Book {
    fn new(bucket_count: usize) -> Self {
        Self {
            bucket_count,
            tallies: BTreeMap::new(),
        }
    }

    fn tally(&mut self, currency: Currency) -> &mut Tally {
        let bucket_count = self.bucket_count;
        self.tallies.entry(currency).or_insert_with(|| Tally {
            cash: 0,
            outflows: 0,
            goals: 0,
            inflows: 0,
            floor: 0,
            months: (0..bucket_count).map(|_| MonthTally::default()).collect(),
        })
    }
}

/// Money that isn't the household's never counts, and that is the reason whatever else
/// holds, since a flag can't change it (spec §8.11 B4, B8). A closed card's debt does not
/// close with it, so one that still owes counts, unless the household has switched it out.
fn not_counted(account: &AccountInput) -> Option<NotCountedReason> {
    let spendable = matches!(
        account.liquidity,
        LiquidityClass::Cash | LiquidityClass::ShortTermLiability
    );
    let owing = account.liquidity == LiquidityClass::ShortTermLiability
        && account
            .balance
            .is_some_and(|balance| balance.minor_units < 0);
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

/// Whether the number reads this account: its balance is summed, or spending linked to a goal
/// on it lowers a goal's need. A feed that stalls on any other account changes nothing.
fn read_by_number(account: &AccountInput) -> bool {
    not_counted(account).is_none() || account.goal_spending
}

fn account_line(account: &AccountInput, book: &mut Book) -> AccountLine {
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
fn dated_line(
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
fn goal_line(
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

fn add_floor(input: &ForecastInput, book: &mut Book) -> Result<(), ForecastError> {
    match input.floor {
        None => {}
        Some(SafetyFloor::Fixed { amount }) => {
            book.tally(amount.currency).floor = i128::from(amount.minor_units);
        }
        Some(SafetyFloor::MonthsOfOutflows { months }) => {
            let end_exclusive = months_after(input.as_of, months);
            for item in &input.items {
                let recurring = matches!(item.schedule, PlanSchedule::Recurring { .. });
                if !recurring || item.direction != PlanDirection::Outflow {
                    continue;
                }
                let dates = item.schedule.occurrences(input.as_of, end_exclusive);
                let held = i128::from(item.amount.minor_units) * i128::from(count(&dates)?);
                // A row exists only where this holds money: an item with no
                // occurrence in these months puts no currency in use.
                if held != 0 {
                    book.tally(item.amount.currency).floor += held;
                }
            }
        }
    }
    Ok(())
}

fn blockers(input: &ForecastInput, accounts: &[AccountLine], book: &Book) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    if book.tallies.len() > 1 {
        blockers.push(Blocker::MixedCurrencies {
            currencies: book.tallies.keys().copied().collect(),
        });
    }
    if input.floor.is_none() {
        blockers.push(Blocker::FloorNotSet);
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
            blockers.push(Blocker::BalanceMissing {
                account: line.account,
                name: line.name.clone(),
            });
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
        let needed = tally.outflows + tally.goals + tally.floor - inflows_counted;
        totals.push(CurrencyTotals {
            currency,
            cash_available: money(tally.cash, currency)?,
            planned_outflows: money(tally.outflows, currency)?,
            goals: money(tally.goals, currency)?,
            floor: money(tally.floor, currency)?,
            planned_inflows: money(inflows_counted, currency)?,
            excluded_inflows: money(tally.inflows - inflows_counted, currency)?,
            cash_needed: money(needed, currency)?,
            surplus: if no_surplus {
                None
            } else {
                Some(money(tally.cash - needed, currency)?)
            },
        });

        let mut projected = tally.cash;
        let mut dipped = false;
        for (bucket, month) in months.iter_mut().zip(tally.months) {
            projected -= month.outflows;
            if input.include_income {
                projected += month.inflows;
            }
            let projected_cash = money(projected, currency)?;
            if input.include_income && input.floor.is_some() && !dipped && projected < tally.floor {
                dipped = true;
                cautions.push(Caution::CashDipsBelowFloor {
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

fn cautions(
    input: &ForecastInput,
    items: &[ItemLine],
    reduction: &Reduction,
    book: &Book,
    counted: &BTreeSet<AccountId>,
) -> Result<Vec<Caution>, ForecastError> {
    let mut cautions = Vec::new();
    if input.as_of != input.today {
        cautions.push(Caution::AsOfNotToday { today: input.today });
    }
    for account in input.accounts.iter().filter(|a| not_counted(a).is_none()) {
        account_cautions(account, input, &mut cautions);
    }
    cautions.extend(
        input
            .unbound
            .iter()
            .map(|unbound| Caution::UnboundProviderAccount {
                provider_account: unbound.provider_account,
                connection: unbound.connection,
                name: unbound.name.clone(),
                institution: unbound.institution.clone(),
                currency_raw: unbound.currency_raw.clone(),
                currency_supported: Currency::parse(&unbound.currency_raw).is_ok(),
            }),
    );
    // Only a connection that could change the number: one that has no provider account yet
    // (a first sync that failed), one with provider accounts still unbound, or one serving an
    // account the number reads. The rest show as failing where connections are listed.
    let read: BTreeSet<AccountId> = input
        .accounts
        .iter()
        .filter(|account| read_by_number(account))
        .map(|account| account.id)
        .collect();
    cautions.extend(
        input
            .feeds
            .iter()
            .filter(|feed| {
                !feed.has_accounts
                    || feed.serves.iter().any(|id| read.contains(id))
                    || input
                        .unbound
                        .iter()
                        .any(|unbound| unbound.connection == feed.connection)
            })
            .map(|feed| Caution::ConnectionNeedsAttention {
                connection: feed.connection,
                label: feed.label.clone(),
                status: feed.status,
            }),
    );

    for line in items {
        item_cautions(line, input, counted, &mut cautions);
    }
    // From the items and not their lines: a goal the links have fully paid has no line, which
    // is when they may have overstated it most.
    cautions.extend(
        input
            .items
            .iter()
            .filter(|item| {
                item.goal_linked_both_sides
                    && matches!(
                        item.schedule,
                        PlanSchedule::Goal {
                            completed: false,
                            ..
                        }
                    )
            })
            .map(|item| Caution::GoalLinkedBothSides { goal: item.source }),
    );
    for (&(category, currency), gap) in &reduction.not_seen {
        cautions.push(Caution::BillNotSeen {
            category,
            amount: money(gap.amount, currency)?,
            items: gap.items.clone(),
        });
    }
    cautions.extend(reduction.bill_cautions.iter().cloned());
    if let Some(SafetyFloor::MonthsOfOutflows { months }) = input.floor {
        for (&currency, tally) in &book.tallies {
            if tally.floor == 0 {
                cautions.push(Caution::MonthsFloorIsZero { currency, months });
            }
        }
    }
    loose_end_cautions(input, reduction, &mut cautions);
    cautions.extend(
        input
            .newer_spending
            .iter()
            .filter(|newer| newer.total_minor > 0 || newer.pending_minor > 0)
            .map(|newer| Caution::SpendingNewerThanBalance {
                currency: newer.currency,
                total: Money {
                    minor_units: newer.total_minor,
                    currency: newer.currency,
                },
                pending: Money {
                    minor_units: newer.pending_minor,
                    currency: newer.currency,
                },
            }),
    );
    Ok(cautions)
}

fn account_cautions(account: &AccountInput, input: &ForecastInput, cautions: &mut Vec<Caution>) {
    if let Some(balance) = account.balance {
        if account.closed {
            cautions.push(Caution::ClosedLiabilityOwing {
                account: account.id,
                balance: Money {
                    minor_units: balance.minor_units,
                    currency: account.currency,
                },
            });
        }
        // At the day the forecast describes, or today if that is yet to come, since a
        // balance can't be fresher than today. A date after it has no age.
        let age_days = balance
            .on
            .until(input.as_of.min(input.today))
            .map_or(0, |age| age.get_days());
        if let Ok(age_days) = u32::try_from(age_days)
            && age_days > u32::from(input.stale_after_days)
        {
            cautions.push(Caution::BalanceStale {
                account: account.id,
                on: balance.on,
                age_days,
            });
        }
        if account.liquidity == LiquidityClass::ShortTermLiability && balance.minor_units > 0 {
            cautions.push(Caution::LiabilityBalancePositive {
                account: account.id,
                balance: Money {
                    minor_units: balance.minor_units,
                    currency: account.currency,
                },
            });
        }
    }
    cautions.extend(
        account
            .feed_problems
            .iter()
            .map(|problem| Caution::AccountFeedProblem {
                account: account.id,
                provider_account: problem.provider_account,
                reason: problem.reason,
            }),
    );
}

/// A hint at an uncounted account matters only if the item could reach the number,
/// which an inflow does not with income off. A goal listed here counts something,
/// so a finished or fully spent one raises nothing.
fn item_cautions(
    line: &ItemLine,
    input: &ForecastInput,
    counted: &BTreeSet<AccountId>,
    cautions: &mut Vec<Caution>,
) {
    let only_shown = line.gross.minor_units == 0 && line.unpaid.minor_units == 0;
    let in_the_number =
        !only_shown && (line.direction == PlanDirection::Outflow || input.include_income);
    if let Some(account) = line.account_hint
        && in_the_number
        && !counted.contains(&account)
    {
        cautions.push(Caution::ItemHintNotCounted {
            item: line.source,
            account,
            effect: if line.direction == PlanDirection::Outflow {
                HintEffect::StillNeeded
            } else {
                HintEffect::LeftOut
            },
        });
    }
    let Some(goal) = line.goal else {
        return;
    };
    if goal.due_on < input.as_of {
        cautions.push(Caution::GoalOverdue {
            goal: line.source,
            due_on: goal.due_on,
        });
    }
    if let Some(category) = line.category {
        let month = Span::new().months(1);
        let earliest = goal.due_on.checked_sub(month).unwrap_or(Date::MIN);
        let latest = goal.due_on.checked_add(month).unwrap_or(Date::MAX);
        for item in &input.items {
            if let PlanSchedule::OneOff { on } = item.schedule
                && item.direction == PlanDirection::Outflow
                && item.category == Some(category)
                && (earliest..=latest).contains(&on)
                && !settled::is_settled(item, on)
            {
                cautions.push(Caution::GoalOneOffOverlap {
                    goal: line.source,
                    one_off: item.source,
                    category,
                });
            }
        }
    }
}

/// This month's unsettled spending only matters where it could still change the
/// number. Uncategorized money is more spending, which only helps a budget or closes
/// a gap. A duplicate is spending that never happened, which pays no bill, but can shrink a
/// budget.
fn loose_end_cautions(input: &ForecastInput, reduction: &Reduction, cautions: &mut Vec<Caution>) {
    let mut possible_duplicates = 0_u32;
    for loose in &input.loose_ends {
        let more_spending_matters = reduction
            .spending_sensitive_currencies
            .contains(&loose.currency);
        if more_spending_matters {
            possible_duplicates = possible_duplicates.saturating_add(loose.possible_duplicates);
        }
        if more_spending_matters && loose.uncategorized_count > 0 {
            cautions.push(Caution::UncategorizedSpending {
                currency: loose.currency,
                count: loose.uncategorized_count,
                total: Money {
                    minor_units: loose.uncategorized_minor,
                    currency: loose.currency,
                },
            });
        }
    }
    if possible_duplicates > 0 {
        cautions.push(Caution::PossibleDuplicates {
            count: possible_duplicates,
        });
    }
    cautions.extend(
        input
            .last_month_loose_ends
            .iter()
            .filter(|loose| {
                reduction
                    .last_month_unpaid_currencies
                    .contains(&loose.currency)
            })
            .map(|loose| Caution::LastMonthLooseEnds {
                currency: loose.currency,
                uncategorized_count: loose.uncategorized_count,
                uncategorized_total: Money {
                    minor_units: loose.uncategorized_minor,
                    currency: loose.currency,
                },
                possible_duplicates: loose.possible_duplicates,
            }),
    );
    cautions.extend(
        input
            .links_with_leg_removed
            .iter()
            .map(|&link| Caution::TransferLegRemoved { link }),
    );
}

#[cfg(test)]
mod tests;
