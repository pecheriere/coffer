//! Free money (money redesign §4.3, §4.4, §10.1): the cash nobody has claimed yet, per currency.
//!
//! ```text
//! still need = what must pay items still need from today through today + 30 days
//!            + what essentials and flexible items still need this calendar month
//! set aside  = what active goals have set aside by today, less what was spent from them
//! before     = cash available − still need − set aside
//! held       = before, between 0 and the emergency fund target
//! free money = before − target, never below 0
//! shortfall  = −before, never below 0
//! ```
//!
//! It reads the forecast's pieces rather than copying them: the cash the forecast counts, the
//! shared reduction (so a bill paid, settled or paid up to a week early needs nothing), what an
//! item still needs ([`still_needed`]), each currency's emergency fund, and the blockers and
//! cautions of a one-month forecast at the same day. Planned income never counts: free money is
//! about cash that exists.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::lines::still_needed;
use super::reduction::{self, ItemAdjustment};
use super::{
    Blocker, Caution, ForecastError, ForecastInput, ForecastStatus, ItemInput, compute_forecast,
    emergency_fund, money,
};
use crate::money::Currency;
use crate::plan::{PlanDirection, PlanSchedule, PlanTier, goal_need};

mod types;

pub use types::{FreeMoney, FreeMoneyCurrency, StillNeed};

/// How many days after today a must pay occurrence still counts, that day included.
pub const MUST_PAY_DAYS: i64 = 30;

/// One currency's running amounts, in minor units.
#[derive(Default, PartialEq, Eq)]
struct Row {
    cash: i128,
    must_pay_this_month: i128,
    must_pay_coming: i128,
    essentials: i128,
    flexible: i128,
    set_aside: i128,
}

/// Computes free money for `input` at `input.as_of`, which should be `input.today`: a different
/// day keeps the forecast's `as_of_not_today` caution. Whatever `horizon_months` and
/// `include_income` say, it looks at the window above and counts no income.
///
/// # Errors
/// [`ForecastError::OutOfRange`] when a total doesn't fit in `i64`, or the window runs past the
/// calendar's end.
pub fn compute_free_money(input: &ForecastInput) -> Result<FreeMoney, ForecastError> {
    let today = input.as_of;
    let next_month = today
        .first_of_month()
        .checked_add(Span::new().months(1))
        .map_err(|_| ForecastError::OutOfRange)?;
    let must_pay_through = today
        .checked_add(Span::new().days(MUST_PAY_DAYS))
        .map_err(|_| ForecastError::OutOfRange)?;
    let must_pay_until = must_pay_through
        .tomorrow()
        .map_err(|_| ForecastError::OutOfRange)?;

    let forecast = compute_forecast(&ForecastInput {
        horizon_months: 1,
        include_income: false,
        ..input.clone()
    })?;
    let mut rows: BTreeMap<Currency, Row> = BTreeMap::new();
    for totals in &forecast.totals {
        rows.entry(totals.currency).or_default().cash =
            i128::from(totals.cash_available.minor_units);
    }

    // The reduction caps budgets at the month's end and paid-ahead bills at a week on its own,
    // so the longer end only lets it see the whole must pay window.
    let reduction = reduction::reduce(input, must_pay_until)?;
    for (item, adjustment) in input.items.iter().zip(&reduction.by_item) {
        let claim = claim(item, adjustment, today, next_month, must_pay_until)?;
        if claim != Row::default() {
            let row = rows.entry(item.amount.currency).or_default();
            row.must_pay_this_month += claim.must_pay_this_month;
            row.must_pay_coming += claim.must_pay_coming;
            row.essentials += claim.essentials;
            row.flexible += claim.flexible;
            row.set_aside += claim.set_aside;
        }
    }

    let unblocked = |currency: Currency| {
        !forecast.blockers.iter().any(|blocker| match blocker {
            Blocker::NoCountedAccounts => true,
            Blocker::BalanceMissing { account, .. } => input
                .accounts
                .iter()
                .any(|found| found.id == *account && found.currency == currency),
            Blocker::MixedCurrencies { .. } => false,
        })
    };
    let in_forecast: BTreeSet<Currency> = forecast.totals.iter().map(|row| row.currency).collect();
    // With income off, the forecast raises nothing about income, so each of its cautions
    // speaks to free money too.
    let mut cautions: Vec<Caution> = forecast.cautions;
    let mut currencies = Vec::with_capacity(rows.len());
    for (currency, row) in rows {
        let fund = emergency_fund(input, currency)?;
        // The forecast already said it of a currency it has in use.
        if !in_forecast.contains(&currency) {
            cautions.extend(fund.caution);
        }
        currencies.push(answer(
            input,
            currency,
            &row,
            fund.minor_units,
            unblocked(currency),
        )?);
    }

    let blockers: Vec<Blocker> = forecast
        .blockers
        .into_iter()
        .filter(|blocker| !matches!(blocker, Blocker::MixedCurrencies { .. }))
        .collect();
    let status = if currencies.iter().all(|row| row.free_money.is_none()) {
        ForecastStatus::Unavailable
    } else if blockers.is_empty() && cautions.is_empty() {
        ForecastStatus::Firm
    } else {
        ForecastStatus::Provisional
    };
    Ok(FreeMoney {
        as_of: today,
        must_pay_through,
        status,
        currencies,
        blockers,
        cautions,
    })
}

/// What `item` claims of its currency's cash today. A goal claims what it has set aside, a
/// finished one nothing, and income nothing. An outflow claims what it still needs: must pay,
/// and an outflow with no tier, through the must pay window; essentials and flexible this month.
fn claim(
    item: &ItemInput,
    adjustment: &ItemAdjustment,
    today: Date,
    next_month: Date,
    must_pay_until: Date,
) -> Result<Row, ForecastError> {
    let mut claim = Row::default();
    match item.schedule {
        PlanSchedule::Goal {
            completed: true, ..
        } => {}
        PlanSchedule::Goal {
            start_on, due_on, ..
        } => {
            claim.set_aside = i128::from(goal_need(
                item.amount.minor_units,
                item.goal_spent_minor,
                start_on,
                due_on,
                today,
            ));
        }
        PlanSchedule::Recurring { .. } | PlanSchedule::OneOff { .. }
            if item.direction == PlanDirection::Inflow => {}
        PlanSchedule::Recurring { .. } | PlanSchedule::OneOff { .. } => {
            let this_month = still_needed(item, adjustment, today, next_month)?.needed;
            match item.tier {
                Some(PlanTier::Essentials) => claim.essentials = this_month,
                Some(PlanTier::Flexible) => claim.flexible = this_month,
                Some(PlanTier::MustPay | PlanTier::SavingToward) | None => {
                    let through = still_needed(item, adjustment, today, must_pay_until)?.needed;
                    claim.must_pay_this_month = this_month;
                    claim.must_pay_coming = through - this_month;
                }
            }
        }
    }
    Ok(claim)
}

/// One currency's answer from its running amounts and its emergency fund in minor units.
fn answer(
    input: &ForecastInput,
    currency: Currency,
    row: &Row,
    target: i128,
    has_number: bool,
) -> Result<FreeMoneyCurrency, ForecastError> {
    let still_need = row.must_pay_this_month + row.must_pay_coming + row.essentials + row.flexible;
    let before = row.cash - still_need - row.set_aside;
    let number = |minor_units: i128| -> Result<_, ForecastError> {
        has_number.then(|| money(minor_units, currency)).transpose()
    };
    Ok(FreeMoneyCurrency {
        currency,
        cash_available: money(row.cash, currency)?,
        still_need: StillNeed {
            must_pay_this_month: money(row.must_pay_this_month, currency)?,
            must_pay_coming: money(row.must_pay_coming, currency)?,
            essentials: money(row.essentials, currency)?,
            flexible: money(row.flexible, currency)?,
            total: money(still_need, currency)?,
        },
        set_aside: money(row.set_aside, currency)?,
        emergency_fund: input.emergency_funds.get(&currency).copied(),
        emergency_fund_target: money(target, currency)?,
        emergency_fund_held: number(before.clamp(0, target.max(0)))?,
        free_money: number((before - target).max(0))?,
        shortfall: number((-before).max(0))?,
    })
}
