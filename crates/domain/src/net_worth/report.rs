//! The report: the daily values at each period's end, added up (spec §8.10).
//!
//! Each amount was rounded on its own, so a total is the plain sum of its rows
//! and any breakdown adds up to it (N4). It answers from `input.accounts` alone:
//! a row or a date of anything else is ignored (N2).

use std::collections::BTreeMap;

use jiff::civil::Date;

use super::daily::DailyValue;
use super::periods::{PeriodEnd, period_ends};
use super::types::{
    AccountPoint, AccountSeries, ConversionState, LiquidityLine, NetWorthBlocker, NetWorthCaution,
    NetWorthError, NetWorthInput, NetWorthPoint, NetWorthReport, NetWorthStatus, NotCountedReason,
    RateMissingReason, ReportAccount,
};
use crate::accounts::LiquidityClass;
use crate::fx::{RATE_STALE_AFTER_DAYS, is_euro};
use crate::ids::AccountId;
use crate::money::{Currency, Money};

/// The order a point lists its liquidity lines in.
const CLASSES: [LiquidityClass; 5] = [
    LiquidityClass::Cash,
    LiquidityClass::ShortTermLiability,
    LiquidityClass::Invested,
    LiquidityClass::Illiquid,
    LiquidityClass::LongTermLiability,
];

/// One included account's row on one point.
struct Line<'a> {
    /// Its position in `input.accounts`.
    index: usize,
    holder: &'a ReportAccount,
    row: &'a DailyValue,
    amount: Option<Money>,
    counted: bool,
}

/// An included account with no row on a point's day, though it had values by
/// then: a hole in the daily values, which must not read as money leaving.
struct Absent<'a> {
    /// Its position in `input.accounts`.
    index: usize,
    holder: &'a ReportAccount,
}

/// A period holding at least one counted row or a hole.
struct Day<'a> {
    period: PeriodEnd,
    lines: Vec<Line<'a>>,
    absent: Vec<Absent<'a>>,
}

/// Computes the report for `input`, whose points are the periods of
/// `[from, min(to, today, through)]` holding a counted balance.
///
/// With `through` absent nothing has been filled, so it answers as while
/// rebuilding.
///
/// # Errors
/// Returns [`NetWorthError`] when the range is refused, which depends only on
/// `from`, `to`, `today` and `interval`, or a total does not fit in `i64`.
pub fn net_worth_report(input: &NetWorthInput) -> Result<NetWorthReport, NetWorthError> {
    let answerable = input.to.min(input.today);
    period_ends(input.from, answerable, input.interval)?;

    let through = match input.through {
        Some(through) if !input.rebuild_needed => through,
        _ => {
            return Ok(report(
                input,
                Vec::new(),
                Vec::new(),
                rebuilding(),
                Vec::new(),
            ));
        }
    };
    let end = answerable.min(through);
    let periods = if input.from <= end {
        period_ends(input.from, end, input.interval)?
    } else {
        Vec::new()
    };

    let days = gather(input, &periods);
    let points = days
        .iter()
        .map(|day| point(day, input.display))
        .collect::<Result<Vec<_>, _>>()?;
    let accounts = series(input, &days);
    let blockers = blockers(input, &periods, &days);
    let cautions = cautions(input, through, &periods, &days);
    Ok(report(input, points, accounts, blockers, cautions))
}

fn rebuilding() -> Vec<NetWorthBlocker> {
    vec![NetWorthBlocker::Rebuilding]
}

fn report(
    input: &NetWorthInput,
    points: Vec<NetWorthPoint>,
    accounts: Vec<AccountSeries>,
    blockers: Vec<NetWorthBlocker>,
    cautions: Vec<NetWorthCaution>,
) -> NetWorthReport {
    let status = if !blockers.is_empty() {
        NetWorthStatus::Unavailable
    } else if cautions.is_empty() {
        NetWorthStatus::Firm
    } else {
        NetWorthStatus::Provisional
    };
    NetWorthReport {
        display_currency: input.display,
        interval: input.interval,
        from: input.from,
        to: input.to,
        through: input.through,
        status,
        points,
        accounts,
        blockers,
        cautions,
    }
}

/// The rows of included accounts on each period's last day and the holes among
/// them, leaving out a period holding neither a counted row nor a hole.
fn gather<'a>(input: &'a NetWorthInput, periods: &[PeriodEnd]) -> Vec<Day<'a>> {
    let included: BTreeMap<_, _> = input
        .accounts
        .iter()
        .enumerate()
        .filter(|(_, holder)| holder.include_in_net_worth)
        .map(|(index, holder)| (holder.id, (index, holder)))
        .collect();
    let mut by_day: BTreeMap<Date, Vec<Line<'a>>> = periods
        .iter()
        .map(|period| (period.on, Vec::new()))
        .collect();
    for row in &input.rows {
        let Some(&(index, holder)) = included.get(&row.account) else {
            continue;
        };
        let Some(lines) = by_day.get_mut(&row.on) else {
            continue;
        };
        lines.push(Line {
            index,
            holder,
            row,
            amount: amount(row, input.display),
            counted: counts_on_its_day(holder, row),
        });
    }
    let first_on: BTreeMap<_, _> = input.first_on.iter().copied().collect();
    periods
        .iter()
        .filter_map(|period| {
            let mut lines = by_day.remove(&period.on)?;
            lines.sort_by_key(|line| line.index);
            let mut absent: Vec<Absent<'a>> = included
                .iter()
                .filter(|(id, _)| first_on.get(*id).is_some_and(|first| *first <= period.on))
                .filter(|(_, (index, _))| !lines.iter().any(|line| line.index == *index))
                .map(|(_, &(index, holder))| Absent { index, holder })
                .collect();
            absent.sort_by_key(|hole| hole.index);
            let holds_something = !absent.is_empty() || lines.iter().any(|line| line.counted);
            holds_something.then_some(Day {
                period: *period,
                lines,
                absent,
            })
        })
        .collect()
}

/// N9: closed, it counts up to the day before it closed, unless it still owes.
fn counts_on_its_day(holder: &ReportAccount, row: &DailyValue) -> bool {
    holder
        .closed_on
        .is_none_or(|closed_on| row.on < closed_on || row.balance_minor < 0)
}

/// The row's amount, only if it is in the currency being summed: the store
/// refuses a row in another, and a sum must not add other units even so.
fn amount(row: &DailyValue, display: Option<Currency>) -> Option<Money> {
    let currency = display.filter(|display| row.display == Some(*display))?;
    Some(Money {
        minor_units: row.display_minor?,
        currency,
    })
}

fn point(day: &Day<'_>, display: Option<Currency>) -> Result<NetWorthPoint, NetWorthError> {
    let counted = || day.lines.iter().filter(|line| line.counted);
    let mut natives: BTreeMap<Currency, i128> = BTreeMap::new();
    for line in counted() {
        let sum = natives.entry(line.holder.currency).or_default();
        *sum = sum
            .checked_add(i128::from(line.row.balance_minor))
            .ok_or(NetWorthError::OutOfRange)?;
    }
    let mut by_liquidity = Vec::new();
    for liquidity in CLASSES {
        let mut in_class = counted()
            .filter(|line| line.holder.liquidity == liquidity)
            .peekable();
        let holed = day
            .absent
            .iter()
            .any(|hole| hole.holder.liquidity == liquidity);
        if holed {
            by_liquidity.push(LiquidityLine {
                liquidity,
                amount: None,
            });
        } else if in_class.peek().is_some() {
            by_liquidity.push(LiquidityLine {
                liquidity,
                amount: sum_of(in_class, display)?,
            });
        }
    }
    let total = if day.absent.is_empty() {
        sum_of(counted(), display)?
    } else {
        None
    };
    let mut no_amount: Vec<_> = counted()
        .filter(|line| display.is_some() && line.amount.is_none())
        .map(|line| (line.index, line.holder.id))
        .chain(day.absent.iter().map(|hole| (hole.index, hole.holder.id)))
        .collect();
    no_amount.sort_by_key(|&(index, _)| index);
    Ok(NetWorthPoint {
        on: day.period.on,
        period_start: day.period.start,
        partial: day.period.partial,
        total,
        by_liquidity,
        by_currency: natives
            .into_iter()
            .map(|(currency, sum)| money(sum, currency))
            .collect::<Result<_, _>>()?,
        unconverted: no_amount.into_iter().map(|(_, id)| id).collect(),
    })
}

/// The lines' amounts added up: `None` when any has none, or there is no
/// currency to add them in. Adding in `i128`, their order can't refuse a sum
/// that fits.
fn sum_of<'a, 'b: 'a>(
    lines: impl Iterator<Item = &'a Line<'b>>,
    display: Option<Currency>,
) -> Result<Option<Money>, NetWorthError> {
    let Some(currency) = display else {
        return Ok(None);
    };
    let mut sum = 0_i128;
    for line in lines {
        let Some(amount) = line.amount else {
            return Ok(None);
        };
        sum = sum
            .checked_add(i128::from(amount.minor_units))
            .ok_or(NetWorthError::OutOfRange)?;
    }
    money(sum, currency).map(Some)
}

fn money(minor: i128, currency: Currency) -> Result<Money, NetWorthError> {
    Ok(Money {
        minor_units: i64::try_from(minor).map_err(|_| NetWorthError::OutOfRange)?,
        currency,
    })
}

fn series(input: &NetWorthInput, days: &[Day<'_>]) -> Vec<AccountSeries> {
    input
        .accounts
        .iter()
        .enumerate()
        .map(|(index, holder)| AccountSeries {
            account: holder.id,
            name: holder.name.clone(),
            liquidity: holder.liquidity,
            currency: holder.currency,
            counted: holder.include_in_net_worth,
            not_counted: (!holder.include_in_net_worth).then_some(NotCountedReason::ExcludedByFlag),
            first_on: input
                .first_on
                .iter()
                .find(|(id, _)| *id == holder.id)
                .map(|&(_, on)| on),
            closed_on: holder.closed_on,
            points: if input.account_points {
                account_points(index, days)
            } else {
                Vec::new()
            },
        })
        .collect()
}

fn account_points(index: usize, days: &[Day<'_>]) -> Vec<AccountPoint> {
    days.iter()
        .flat_map(|day| day.lines.iter())
        .filter(|line| line.index == index)
        .map(|line| AccountPoint {
            on: line.row.on,
            balance: Money {
                minor_units: line.row.balance_minor,
                currency: line.holder.currency,
            },
            balance_on: line.row.balance_on,
            balance_source: line.row.balance_source,
            counted: line.counted,
            amount: line.amount,
            rate_on: line.row.rate_on,
            conversion: line.row.conversion,
        })
        .collect()
}

fn blockers(
    input: &NetWorthInput,
    periods: &[PeriodEnd],
    days: &[Day<'_>],
) -> Vec<NetWorthBlocker> {
    let mut blockers = Vec::new();
    if input.display.is_none() {
        blockers.push(NetWorthBlocker::DisplayCurrencyNotSet);
    }
    // Periods asked for and none counted is the same answer as no account: a
    // range wholly after `through` has none to ask for, and a caution of its own.
    let none_included = !input.accounts.iter().any(|one| one.include_in_net_worth);
    if none_included || (days.is_empty() && !periods.is_empty()) {
        blockers.push(NetWorthBlocker::NoCountedAccounts);
    }
    blockers
}

fn cautions(
    input: &NetWorthInput,
    through: Date,
    periods: &[PeriodEnd],
    days: &[Day<'_>],
) -> Vec<NetWorthCaution> {
    let mut cautions = Vec::new();
    if input.to.min(input.today) > through {
        cautions.push(NetWorthCaution::ToDateNotYetFilled { through });
    }
    cautions.extend(nothing_counted_after(periods, days));
    cautions.extend(rate_cautions(input, days));
    cautions.extend(hole_cautions(days));
    cautions.extend(line_cautions(input, days));
    cautions
}

/// When the last point stops short of the range's last period: a reader would
/// otherwise take it as the value now. No point at all is a blocker instead.
fn nothing_counted_after(periods: &[PeriodEnd], days: &[Day<'_>]) -> Option<NetWorthCaution> {
    let last_point = days.last()?.period.on;
    let next = periods.iter().find(|period| period.on > last_point)?;
    Some(NetWorthCaution::NothingCountedAfterLastPoint {
        from: next.start,
        to: periods.last()?.on,
    })
}

/// One caution per account with a hole, from its first to its last, in input
/// order.
fn hole_cautions(days: &[Day<'_>]) -> Vec<NetWorthCaution> {
    let mut holes = BTreeMap::new();
    for day in days {
        let on = day.period.on;
        for hole in &day.absent {
            holes
                .entry(hole.index)
                .and_modify(|seen: &mut (AccountId, Date, Date)| seen.2 = on)
                .or_insert((hole.holder.id, on, on));
        }
    }
    holes
        .into_values()
        .map(
            |(account, first_on, last_on)| NetWorthCaution::DailyValueMissing {
                account,
                first_on,
                last_on,
            },
        )
        .collect()
}

/// Rate cautions, one per currency, sorted by currency.
fn rate_cautions(input: &NetWorthInput, days: &[Day<'_>]) -> Vec<NetWorthCaution> {
    let Some(display) = input.display else {
        return Vec::new();
    };
    let mut missing: BTreeMap<Currency, (RateMissingReason, Date, Date)> = BTreeMap::new();
    let mut stale: BTreeMap<Currency, (Date, Date, u32)> = BTreeMap::new();
    for day in days {
        let on = day.period.on;
        for line in day.lines.iter().filter(|line| line.counted) {
            let held = line.holder.currency;
            if line.amount.is_none() {
                if line.row.conversion == ConversionState::TooLarge {
                    continue;
                }
                for (currency, reason) in missing_legs(held, display, input) {
                    missing
                        .entry(currency)
                        .and_modify(|seen| seen.2 = on)
                        .or_insert((reason, on, on));
                }
            } else if let Some((rate_on, age_days)) = stale_rate(line.row) {
                let worst = stale
                    .entry(foreign_leg(held, display))
                    .or_insert((on, rate_on, age_days));
                if age_days > worst.2 {
                    *worst = (on, rate_on, age_days);
                }
            }
        }
    }
    let missing = missing
        .into_iter()
        .map(
            |(currency, (reason, first_on, last_on))| NetWorthCaution::RateMissing {
                currency,
                reason,
                first_on,
                last_on,
            },
        );
    let stale =
        stale.into_iter().map(
            |(currency, (on, rate_on, age_days))| NetWorthCaution::RateStale {
                currency,
                on,
                rate_on,
                age_days,
            },
        );
    missing.chain(stale).collect()
}

/// The row's rate date and age, when that is more than a week older than its day.
fn stale_rate(row: &DailyValue) -> Option<(Date, u32)> {
    let rate_on = row.rate_on?;
    let days_old = rate_on.until(row.on).ok()?.get_days();
    (days_old > RATE_STALE_AFTER_DAYS).then_some((rate_on, u32::try_from(days_old).ok()?))
}

/// The currency to name for a conversion from `held` to `display`: the row keeps
/// one rate date, not which leg it came from. Never the euro, which has no rate.
fn foreign_leg(held: Currency, display: Currency) -> Currency {
    if is_euro(held) { display } else { held }
}

/// The currencies to blame for a missing conversion: those with a reason, or the
/// foreign leg when both have rates, so none is that early.
fn missing_legs(
    held: Currency,
    display: Currency,
    input: &NetWorthInput,
) -> Vec<(Currency, RateMissingReason)> {
    let reason_for = |currency: Currency| {
        input
            .coverage
            .iter()
            .find(|state| state.currency == currency)
            .and_then(|state| state.reason)
    };
    let mut legs: Vec<_> = [held, display]
        .into_iter()
        .filter(|leg| !is_euro(*leg))
        .filter_map(|leg| Some((leg, reason_for(leg)?)))
        .collect();
    if legs.is_empty() {
        legs.push((foreign_leg(held, display), RateMissingReason::NoneThatEarly));
    }
    legs
}

/// Cautions about single accounts, each said once, in input order.
fn line_cautions(input: &NetWorthInput, days: &[Day<'_>]) -> Vec<NetWorthCaution> {
    // Walking the days oldest first, the last one seen is what is kept.
    let mut too_large = BTreeMap::new();
    let mut positive = BTreeMap::new();
    let mut owing = BTreeMap::new();
    for day in days {
        for line in day.lines.iter().filter(|line| line.counted) {
            let row = line.row;
            let balance = Money {
                minor_units: row.balance_minor,
                currency: line.holder.currency,
            };
            if row.conversion == ConversionState::TooLarge {
                too_large.insert(line.index, (line.holder.id, row.on));
            }
            let liability = matches!(
                line.holder.liquidity,
                LiquidityClass::ShortTermLiability | LiquidityClass::LongTermLiability
            );
            if liability && row.balance_minor > 0 {
                positive.insert(line.index, (line.holder.id, balance));
            }
            if line
                .holder
                .closed_on
                .is_some_and(|closed_on| row.on >= closed_on)
            {
                owing.insert(line.index, (line.holder.id, balance));
            }
        }
    }
    let too_large = too_large
        .into_values()
        .map(|(account, on)| NetWorthCaution::AmountTooLarge { account, on });
    let stale = days
        .last()
        .into_iter()
        .flat_map(|day| &day.lines)
        .filter(|line| line.counted)
        .filter_map(|line| {
            let days_old = line.row.balance_on.until(line.row.on).ok()?.get_days();
            let age_days = u32::try_from(days_old).ok()?;
            (age_days > u32::from(input.stale_after_days)).then_some(
                NetWorthCaution::BalanceStale {
                    account: line.holder.id,
                    on: line.row.balance_on,
                    age_days,
                },
            )
        });
    let positive = positive
        .into_values()
        .map(|(account, balance)| NetWorthCaution::LiabilityBalancePositive { account, balance });
    let owing = owing
        .into_values()
        .map(|(account, balance)| NetWorthCaution::ClosedAccountOwing { account, balance });
    too_large
        .chain(stale)
        .chain(positive)
        .chain(owing)
        .collect()
}
