//! Expected values here are worked out by hand from the dates and amounts in
//! each test, never taken from the code under test.

use std::collections::BTreeMap;

use jiff::ToSpan;
use jiff::civil::{Date, date};

use super::{
    AccountInput, BalanceInput, CategorySpent, Caution, CurrencyTotals, Forecast, ForecastInput,
    ForecastStatus, ItemInput, ItemLine, LineSource, NotCountedReason, SpentTransaction,
    bills_looked_at_from, compute_forecast,
};
use crate::accounts::{BelongsTo, LiquidityClass};
use crate::fx::RateBook;
use crate::ids::{AccountId, CategoryId, PlannedItemId, TransactionId};
use crate::money::{Currency, Money};
use crate::plan::{
    IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, SafetyFloor, spending_applies,
};

mod account_cautions;
mod bill_matching;
mod blockers;
mod budget_month;
mod clamp_and_duplicates;
mod goals_and_floor;
mod golden;
mod inflow_window;
mod item_cautions;
mod newer_spending;
mod properties;
mod reduction;
mod rule_filed_transfers;
mod settled;
mod settled_budgets;
mod settling_need;
mod spending_cautions;
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
        spent_transactions: Vec::new(),
        loose_ends: Vec::new(),
        last_month_loose_ends: Vec::new(),
        earliest_transaction: Some(date(2020, 1, 1)),
        newer_spending: Vec::new(),
        rule_filed_transfers: Vec::new(),
        links_with_leg_removed: Vec::new(),
        cross_currency_links: Vec::new(),
        rates: RateBook::default(),
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
        belongs_to: BelongsTo::Household,
        include_in_forecast: true,
        closed: false,
        balance: Some(BalanceInput {
            on: date(2026, 11, 14),
            minor_units: balance.minor_units,
        }),
        feed_problems: Vec::new(),
        goal_spending: false,
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
        goal_linked_both_sides: false,
        settled: BTreeMap::new(),
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

/// `minor` dollars spent in `category` this month.
fn spent(category: CategoryId, minor: i64) -> CategorySpent {
    CategorySpent {
        category,
        currency: currency("USD"),
        spent_minor: minor,
    }
}

fn hypothetical(index: u32, item: ItemInput) -> ItemInput {
    ItemInput {
        source: LineSource::Hypothetical { index },
        ..item
    }
}

/// What a test about this month means by its spending: last month's bills were paid on their
/// dates, so only this month is in question, and unless it lists its own transactions each
/// category's spending is one, the day before `as_of`.
fn run(input: &ForecastInput) -> Forecast {
    let month_start = input.as_of.first_of_month();
    let mut rows = Vec::new();
    for item in &input.items {
        let stored = matches!(item.source, LineSource::PlannedItem { .. });
        let (true, PlanDirection::Outflow, Some(category), false) = (
            stored,
            item.direction,
            item.category,
            item.spending == Some(PlanSpending::Budget),
        ) else {
            continue;
        };
        for on in item
            .schedule
            .occurrences(bills_looked_at_from(input.as_of), month_start)
        {
            rows.push(paid_on(category, item.amount, on));
        }
    }
    if input.spent_transactions.is_empty() {
        let yesterday = input.as_of.yesterday().unwrap().max(month_start);
        for row in input
            .category_spent
            .iter()
            .filter(|row| row.spent_minor > 0)
        {
            let amount = Money {
                minor_units: row.spent_minor,
                currency: row.currency,
            };
            rows.push(paid_on(row.category, amount, yesterday));
        }
    }
    rows.extend(input.spent_transactions.iter().copied());
    run_as_given(&ForecastInput {
        spent_transactions: rows,
        ..input.clone()
    })
}

/// The one `bill_not_seen` `forecast` should hold for `category`, naming the items that owe.
fn not_seen(forecast: &Forecast, category: CategoryId, amount: Money) -> Caution {
    Caution::BillNotSeen {
        category,
        amount,
        items: forecast
            .items
            .iter()
            .filter(|line| line.category == Some(category) && line.unpaid.minor_units != 0)
            .map(|line| line.source)
            .collect(),
    }
}

fn run_as_given(input: &ForecastInput) -> Forecast {
    compute_forecast(input).unwrap()
}

/// A transaction of `amount` in `category`.
fn paid_on(category: CategoryId, amount: Money, on: Date) -> SpentTransaction {
    SpentTransaction {
        transaction: TransactionId::new(),
        category,
        currency: amount.currency,
        posted_on: on,
        spent_minor: amount.minor_units,
        possible_duplicate: false,
    }
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

/// What the forecast says of `held`, beside a counted Checking holding 1,000.
fn beside_checking(held: AccountInput) -> Forecast {
    run(&ForecastInput {
        accounts: vec![cash("Checking", usd(1_000)), held],
        ..household()
    })
}

/// Each shape, flag switched on so that only `belongs_to` can keep it out, as the table's
/// CHECK wouldn't allow: a closed card still owing is the one that would otherwise count.
#[test]
fn not_household_is_the_reason_whatever_else_holds() {
    let shapes = [
        cash("Cash", usd(5_000)),
        AccountInput {
            liquidity: LiquidityClass::Invested,
            ..cash("Invested", usd(5_000))
        },
        AccountInput {
            closed: true,
            ..cash("Closed", usd(5_000))
        },
        AccountInput {
            liquidity: LiquidityClass::ShortTermLiability,
            closed: true,
            ..cash("Closed card", usd(-5_000))
        },
    ];
    for belongs_to in [BelongsTo::SomeoneElse, BelongsTo::GivenAway] {
        for shape in &shapes {
            let forecast = beside_checking(AccountInput {
                belongs_to,
                ..shape.clone()
            });

            let held = &forecast.accounts[1];
            assert_eq!(held.not_counted, Some(NotCountedReason::NotHousehold));
            assert!(!held.counted, "it stays out");
            assert_eq!(usd_totals(&forecast).cash_available, usd(1_000));
            assert_eq!(forecast.cautions, [], "and raises nothing");
        }
    }
}

#[test]
fn a_household_account_keeps_its_reasons() {
    let shapes = [
        (
            AccountInput {
                liquidity: LiquidityClass::Invested,
                ..cash("Invested", usd(5_000))
            },
            Some(NotCountedReason::NotCash),
        ),
        (
            AccountInput {
                closed: true,
                ..cash("Closed", usd(5_000))
            },
            Some(NotCountedReason::Closed),
        ),
        (
            AccountInput {
                include_in_forecast: false,
                ..cash("Switched out", usd(5_000))
            },
            Some(NotCountedReason::ExcludedByFlag),
        ),
        (cash("Cash", usd(5_000)), None),
        (
            AccountInput {
                liquidity: LiquidityClass::ShortTermLiability,
                closed: true,
                ..cash("Closed card", usd(-5_000))
            },
            None,
        ),
    ];
    for (shape, reason) in shapes {
        let forecast = beside_checking(shape);

        let held = &forecast.accounts[1];
        assert_eq!(held.not_counted, reason);
    }
}
