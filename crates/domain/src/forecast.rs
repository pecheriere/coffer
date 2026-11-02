//! The cash forecast (spec §6.8, §8.9): what is free once the window's planned
//! spending, goals and the safety floor are covered. Pure and exact: amounts add
//! up in `i128`, and any total that does not fit in `i64` refuses the answer.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use crate::accounts::LiquidityClass;
use crate::ids::AccountId;
use crate::money::{Currency, Money};
use crate::plan::{CalendarMonth, PlanDirection, PlanSchedule, SafetyFloor, goal_need};
use reduction::Reduction;

mod reduction;
mod types;

pub use types::{
    AccountInput, AccountLine, BalanceInput, Blocker, CategorySpent, Caution, CurrencyTotals,
    FeedAlert, FeedProblem, FeedProblemReason, FeedStatus, Forecast, ForecastError, ForecastInput,
    ForecastStatus, GoalLine, ItemInput, ItemLine, LineSource, LooseEnds, MonthBucket,
    MonthCurrency, MonthItem, NotCountedReason, UnboundAccount,
};

/// Computes the forecast for `input`, which the caller has validated.
///
/// # Errors
/// Returns [`ForecastError::OutOfRange`] when a total does not fit in `i64`, or
/// `as_of` is before year 0.
pub fn compute_forecast(input: &ForecastInput) -> Result<Forecast, ForecastError> {
    let end_exclusive = months_after(input.as_of, input.horizon_months);
    let buckets = buckets(input.as_of, end_exclusive)?;
    let window = Window {
        as_of: input.as_of,
        end_exclusive,
        buckets: &buckets,
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
    for (item, &reduced_by) in input.items.iter().zip(&reduction.by_item) {
        let line = match item.schedule {
            PlanSchedule::Goal {
                completed: true, ..
            } => None,
            PlanSchedule::Goal {
                start_on, due_on, ..
            } => goal_line(item, start_on, due_on, &window, &mut book)?,
            PlanSchedule::Recurring { .. } | PlanSchedule::OneOff { .. } => dated_line(
                item,
                reduced_by,
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
    let mut cautions = cautions(input, &accounts, &items, &reduction);
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
    })
}

/// `date` plus `months`, clamped to the month's end, and to the calendar's.
fn months_after(date: Date, months: u8) -> Date {
    date.checked_add(Span::new().months(months))
        .unwrap_or(Date::MAX)
}

fn money(minor_units: i128, currency: Currency) -> Result<Money, ForecastError> {
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
    buckets: &'a [Bucket],
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
    /// Every planned inflow in the window, counted or not.
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

fn not_counted(account: &AccountInput) -> Option<NotCountedReason> {
    let spendable = matches!(
        account.liquidity,
        LiquidityClass::Cash | LiquidityClass::ShortTermLiability
    );
    if account.closed {
        Some(NotCountedReason::Closed)
    } else if !spendable {
        Some(NotCountedReason::NotCash)
    } else if !account.include_in_forecast {
        Some(NotCountedReason::ExcludedByFlag)
    } else {
        None
    }
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
    reduced_by: i128,
    include_income: bool,
    window: &Window<'_>,
    month_counts: &mut [i128],
    book: &mut Book,
) -> Result<Option<ItemLine>, ForecastError> {
    let dates = item
        .schedule
        .occurrences(window.as_of, window.end_exclusive);
    let amount = i128::from(item.amount.minor_units);
    let currency = item.amount.currency;
    let occurrences = count(&dates)?;
    let gross = amount * i128::from(occurrences);
    if gross == 0 {
        return Ok(None);
    }

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

    let outflow = item.direction == PlanDirection::Outflow;
    let tally = book.tally(currency);
    if outflow {
        tally.outflows += gross - reduced_by;
    } else {
        tally.inflows += gross;
    }
    for (index, (count, month)) in month_counts.iter().zip(&mut tally.months).enumerate() {
        // The reduction only touches occurrences in the first bucket.
        let reduction = if index == 0 { reduced_by } else { 0 };
        let minor_units = count * amount - reduction;
        if outflow {
            month.outflows += minor_units;
        } else {
            month.inflows += minor_units;
        }
        month.itemize(item.source, minor_units, currency)?;
    }

    let counted = if outflow {
        gross - reduced_by
    } else if include_income {
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
        counted: money(counted, currency)?,
        goal: None,
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
                // A currency whose floor is 0 is not in use, but one that holds
                // something here is, even with nothing in the window: dropping
                // it would hide money the household must keep.
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
    if !accounts.iter().any(|line| line.counted) {
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
    accounts: &[AccountLine],
    items: &[ItemLine],
    reduction: &Reduction,
) -> Vec<Caution> {
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
    cautions.extend(
        input
            .feeds
            .iter()
            .map(|feed| Caution::ConnectionNeedsAttention {
                connection: feed.connection,
                label: feed.label.clone(),
                status: feed.status,
            }),
    );

    let counted: BTreeSet<AccountId> = accounts
        .iter()
        .filter(|line| line.counted)
        .map(|line| line.account)
        .collect();
    for line in items {
        item_cautions(line, input, &counted, &mut cautions);
    }
    loose_end_cautions(input, reduction, &mut cautions);
    cautions
}

fn account_cautions(account: &AccountInput, input: &ForecastInput, cautions: &mut Vec<Caution>) {
    if let Some(balance) = account.balance {
        // At `as_of`, the day the forecast describes. A date after it has no age.
        let age_days = balance
            .on
            .until(input.as_of)
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

/// A hint at an uncounted account matters only if the item reached the number,
/// which an inflow does not with income off. A goal listed here counts something,
/// so a finished or fully spent one raises nothing.
fn item_cautions(
    line: &ItemLine,
    input: &ForecastInput,
    counted: &BTreeSet<AccountId>,
    cautions: &mut Vec<Caution>,
) {
    let in_the_number = line.direction == PlanDirection::Outflow || input.include_income;
    if let Some(account) = line.account_hint
        && in_the_number
        && !counted.contains(&account)
    {
        cautions.push(Caution::ItemHintNotCounted {
            item: line.source,
            account,
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

/// This month's unsettled spending only matters where it could still reduce a
/// recurring occurrence.
fn loose_end_cautions(input: &ForecastInput, reduction: &Reduction, cautions: &mut Vec<Caution>) {
    let mut possible_duplicates = 0_u32;
    for loose in &input.loose_ends {
        if !reduction.reducible_currencies.contains(&loose.currency) {
            continue;
        }
        possible_duplicates = possible_duplicates.saturating_add(loose.possible_duplicates);
        if loose.uncategorized_count > 0 {
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
}

#[cfg(test)]
mod tests;
