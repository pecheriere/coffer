//! Expected values here are worked out by hand from the dates and amounts in
//! each test, never taken from the code under test.

use jiff::ToSpan;
use jiff::civil::{Date, date};

use super::{
    AccountInput, BalanceInput, CurrencyTotals, Forecast, ForecastInput, ForecastStatus, ItemInput,
    ItemLine, LineSource, compute_forecast,
};
use crate::accounts::LiquidityClass;
use crate::ids::{AccountId, CategoryId, PlannedItemId};
use crate::money::{Currency, Money};
use crate::plan::{
    IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, SafetyFloor, spending_applies,
};

mod blockers;
mod cautions;
mod clamp_and_duplicates;
mod goals_and_floor;
mod golden;
mod properties;
mod reduction;
mod window_and_range;

const AS_OF: Date = date(2026, 11, 15);

fn currency(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: currency("USD"),
    }
}

fn eur(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: currency("EUR"),
    }
}

fn fixed_floor(amount: Money) -> SafetyFloor {
    SafetyFloor::Fixed { amount }
}

/// A firm household: one checking account holding 1,000,000 USD as of
/// yesterday, a floor of 0, a three-month horizon and nothing planned.
fn household() -> ForecastInput {
    ForecastInput {
        today: AS_OF,
        as_of: AS_OF,
        horizon_months: 3,
        include_income: false,
        floor: Some(fixed_floor(usd(0))),
        stale_after_days: 7,
        accounts: vec![cash("Checking", usd(1_000_000))],
        items: Vec::new(),
        category_spent: Vec::new(),
        loose_ends: Vec::new(),
        unbound: Vec::new(),
        feeds: Vec::new(),
    }
}

/// A counted cash account with a balance from the day before `AS_OF`.
fn cash(name: &str, balance: Money) -> AccountInput {
    AccountInput {
        id: AccountId::new(),
        name: name.to_owned(),
        currency: balance.currency,
        liquidity: LiquidityClass::Cash,
        include_in_forecast: true,
        closed: false,
        balance: Some(BalanceInput {
            on: date(2026, 11, 14),
            minor_units: balance.minor_units,
        }),
        feed_problems: Vec::new(),
    }
}

fn stored(name: &str, amount: Money, schedule: PlanSchedule) -> ItemInput {
    let direction = PlanDirection::Outflow;
    ItemInput {
        source: LineSource::PlannedItem {
            id: PlannedItemId::new(),
        },
        name: name.to_owned(),
        category: None,
        amount,
        direction,
        account_hint: None,
        schedule,
        spending: spending_applies(direction, &schedule).then_some(PlanSpending::Bill),
        goal_spent_minor: 0,
    }
}

fn every_month(anchor_on: Date) -> PlanSchedule {
    every(1, IntervalUnit::Months, anchor_on)
}

fn every(every: u16, unit: IntervalUnit, anchor_on: Date) -> PlanSchedule {
    PlanSchedule::Recurring {
        every,
        unit,
        anchor_on,
        end_on: None,
    }
}

fn monthly(name: &str, amount: Money, anchor_on: Date, category: CategoryId) -> ItemInput {
    ItemInput {
        category: Some(category),
        ..stored(name, amount, every_month(anchor_on))
    }
}

fn one_off(name: &str, amount: Money, on: Date, category: CategoryId) -> ItemInput {
    ItemInput {
        category: Some(category),
        ..stored(name, amount, PlanSchedule::OneOff { on })
    }
}

fn goal(name: &str, target: Money, start_on: Date, due_on: Date, spent: i64) -> ItemInput {
    ItemInput {
        goal_spent_minor: spent,
        ..stored(
            name,
            target,
            PlanSchedule::Goal {
                start_on,
                due_on,
                completed: false,
            },
        )
    }
}

fn inflow(item: ItemInput) -> ItemInput {
    ItemInput {
        direction: PlanDirection::Inflow,
        spending: None,
        ..item
    }
}

fn budget(item: ItemInput) -> ItemInput {
    ItemInput {
        spending: Some(PlanSpending::Budget),
        ..item
    }
}

fn hypothetical(index: u32, item: ItemInput) -> ItemInput {
    ItemInput {
        source: LineSource::Hypothetical { index },
        ..item
    }
}

fn run(input: &ForecastInput) -> Forecast {
    compute_forecast(input).unwrap()
}

fn totals_of(forecast: &Forecast, code: &str) -> CurrencyTotals {
    *forecast
        .totals
        .iter()
        .find(|row| row.currency == currency(code))
        .unwrap()
}

fn usd_totals(forecast: &Forecast) -> CurrencyTotals {
    totals_of(forecast, "USD")
}

fn line<'a>(forecast: &'a Forecast, name: &str) -> &'a ItemLine {
    forecast
        .items
        .iter()
        .find(|line| line.name == name)
        .unwrap()
}

fn has_line(forecast: &Forecast, name: &str) -> bool {
    forecast.items.iter().any(|line| line.name == name)
}

/// The number is still there, but no longer firm.
fn assert_provisional(forecast: &Forecast) {
    assert_eq!(
        forecast.status,
        ForecastStatus::Provisional,
        "a caution keeps the number but not its firmness"
    );
    assert!(forecast.surplus.is_some(), "the number is still present");
}

fn days_before(date: Date, days: i32) -> Date {
    date.checked_sub(days.days()).unwrap()
}
