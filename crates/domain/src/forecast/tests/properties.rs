//! Random households: the answer must explain itself, whatever goes in.

use std::collections::BTreeMap;

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, any, prop_assert, prop_assert_eq, prop_oneof};
use proptest::{collection, proptest};

use super::{AS_OF, budget, cash, every, household, hypothetical, stored};
use super::{currency, eur, inflow, usd};
use crate::accounts::LiquidityClass;
use crate::forecast::{
    AccountInput, BalanceInput, Blocker, CategorySpent, Caution, Forecast, ForecastInput,
    ForecastStatus, ItemInput, LineSource, LooseEnds, compute_forecast,
};
use crate::ids::CategoryId;
use crate::money::{Currency, Money};
use crate::plan::{
    CalendarMonth, IntervalUnit, PlanDirection, PlanItemType, PlanSchedule, PlanSpending,
    SafetyFloor,
};

const MAX_AMOUNT: i64 = 1_000_000_000;

fn day(offset: i32) -> Date {
    AS_OF.checked_add(offset.days()).unwrap()
}

/// Mostly dollars, so most households have a single number to check.
fn money(amount: impl Strategy<Value = i64>) -> impl Strategy<Value = Money> {
    (amount, 0..10).prop_map(|(minor, pick)| if pick == 0 { eur(minor) } else { usd(minor) })
}

fn account() -> impl Strategy<Value = AccountInput> {
    (
        money(-MAX_AMOUNT..MAX_AMOUNT),
        prop_oneof![
            Just(LiquidityClass::Cash),
            Just(LiquidityClass::Cash),
            Just(LiquidityClass::ShortTermLiability),
            Just(LiquidityClass::Invested),
        ],
        0..8,
        -20..3,
    )
        .prop_map(|(balance, liquidity, flags, age)| AccountInput {
            liquidity,
            include_in_forecast: flags != 0,
            closed: flags == 1,
            balance: (flags != 2).then_some(BalanceInput {
                on: day(age),
                minor_units: balance.minor_units,
            }),
            ..cash("Account", balance)
        })
}

fn schedule() -> impl Strategy<Value = PlanSchedule> {
    prop_oneof![
        (1_u16..4, 0..4, -400..200).prop_map(|(step, unit, offset)| {
            let unit = match unit {
                0 => IntervalUnit::Days,
                1 => IntervalUnit::Weeks,
                2 => IntervalUnit::Months,
                _ => IntervalUnit::Years,
            };
            every(step, unit, day(offset))
        }),
        (-100..400).prop_map(|offset| PlanSchedule::OneOff { on: day(offset) }),
        (-400..400, 1..800, any::<bool>()).prop_map(|(start, length, completed)| {
            PlanSchedule::Goal {
                start_on: day(start),
                due_on: day(start + length),
                completed,
            }
        }),
    ]
}

/// `categories` is the pool items draw on: few, so that they share.
fn item(categories: Vec<CategoryId>) -> impl Strategy<Value = ItemInput> {
    (
        money(1..MAX_AMOUNT),
        schedule(),
        0..categories.len(),
        0..6,
        0..MAX_AMOUNT,
    )
        .prop_map(move |(amount, schedule, category, kind, spent)| {
            let is_goal = matches!(schedule, PlanSchedule::Goal { .. });
            let base = ItemInput {
                category: categories.get(category).copied(),
                goal_spent_minor: if is_goal { spent } else { 0 },
                ..stored("Item", amount, schedule)
            };
            let is_recurring = matches!(base.schedule, PlanSchedule::Recurring { .. });
            match kind {
                0 if !is_goal => inflow(base),
                1 => hypothetical(0, base),
                2 | 3 if is_recurring => budget(base),
                _ => base,
            }
        })
}

/// Dates where a month's length matters, and ordinary ones around them.
fn as_of() -> impl Strategy<Value = Date> {
    prop_oneof![
        Just(AS_OF),
        Just(date(2026, 10, 31)),
        Just(date(2026, 11, 28)),
        Just(date(2026, 11, 29)),
        Just(date(2026, 11, 30)),
        Just(date(2026, 12, 31)),
        Just(date(2027, 1, 28)),
        Just(date(2027, 1, 29)),
        Just(date(2027, 1, 30)),
        Just(date(2027, 1, 31)),
        Just(date(2027, 2, 28)),
        Just(date(2028, 1, 31)),
        Just(date(2028, 2, 29)),
        (-200..300).prop_map(day),
    ]
}

/// Worked out here, apart from the code under test: `as_of` plus the months, and
/// a day more when the month is too short to hold `as_of`'s day.
fn window_end(as_of: Date, months: u8) -> Result<Date, String> {
    let plain = as_of
        .checked_add(i64::from(months).months())
        .map_err(|_| "no window end")?;
    if plain.day() == as_of.day() {
        Ok(plain)
    } else {
        plain.tomorrow().map_err(|_| "no window end".to_owned())
    }
}

fn input() -> impl Strategy<Value = ForecastInput> {
    let categories = vec![CategoryId::new(), CategoryId::new(), CategoryId::new()];
    let spent = collection::vec((0..categories.len(), 0..MAX_AMOUNT, any::<bool>()), 0..4);
    (
        collection::vec(account(), 0..5),
        collection::vec(item(categories.clone()), 0..12),
        spent,
        prop_oneof![
            Just(None),
            (0..MAX_AMOUNT).prop_map(|minor| Some(SafetyFloor::Fixed { amount: usd(minor) })),
            (1_u8..13).prop_map(|months| Some(SafetyFloor::MonthsOfOutflows { months })),
        ],
        1_u8..16,
        any::<bool>(),
        -2..2,
        collection::vec((0_u32..3, 0_u32..3, 0..MAX_AMOUNT), 0..2),
        as_of(),
    )
        .prop_map(
            move |(
                accounts,
                items,
                spent,
                floor,
                horizon_months,
                include_income,
                shift,
                loose,
                as_of,
            )| {
                ForecastInput {
                    today: as_of.checked_add(i64::from(shift).days()).unwrap(),
                    as_of,
                    accounts,
                    items,
                    floor,
                    horizon_months,
                    include_income,
                    category_spent: spent
                        .iter()
                        .filter_map(|&(pick, spent_minor, in_euros)| {
                            Some(CategorySpent {
                                category: *categories.get(pick)?,
                                currency: currency(if in_euros { "EUR" } else { "USD" }),
                                spent_minor,
                            })
                        })
                        .collect(),
                    loose_ends: loose
                        .into_iter()
                        .map(|(count, duplicates, minor)| LooseEnds {
                            currency: currency("USD"),
                            uncategorized_count: count,
                            uncategorized_minor: minor,
                            possible_duplicates: duplicates,
                        })
                        .collect(),
                    ..household()
                }
            },
        )
}

fn wide(money: Money) -> i128 {
    i128::from(money.minor_units)
}

fn in_currency(
    forecast: &Forecast,
    wanted: Currency,
) -> impl Iterator<Item = &crate::forecast::ItemLine> {
    forecast
        .items
        .iter()
        .filter(move |line| line.counted.currency == wanted)
}

/// One category and currency's figures, worked out here in aggregate.
#[derive(Default)]
struct PoolFigures {
    spent: i128,
    /// Stored outflows dated earlier this month.
    earlier: i128,
    /// The part of `earlier` that is recurring bills.
    earlier_bills: i128,
    /// Budgets' occurrences from `as_of` to the month's end.
    remaining_budgets: i128,
}

fn dated(item: &ItemInput, from: Date, to: Date) -> Result<i128, String> {
    let dates = item.schedule.occurrences(from, to).len();
    let dates = i128::try_from(dates).map_err(|_| "too many")?;
    Ok(i128::from(item.amount.minor_units) * dates)
}

/// Checks reductions and unpaid bills against aggregates worked out apart from the
/// walk: the pool (spending less earlier plans, floored at 0) is either all used or
/// covers all that remains, and the gap is earlier bills less spending, floored at 0.
fn check_reduction(input: &ForecastInput, forecast: &Forecast) -> Result<(), String> {
    let month_start = input.as_of.first_of_month();
    let month_end = input
        .as_of
        .last_of_month()
        .tomorrow()
        .map_err(|_| "no next month")?
        .min(window_end(input.as_of, input.horizon_months)?);
    let mut pools: BTreeMap<(CategoryId, Currency), PoolFigures> = BTreeMap::new();
    for spent in &input.category_spent {
        pools
            .entry((spent.category, spent.currency))
            .or_default()
            .spent += i128::from(spent.spent_minor);
    }
    for item in &input.items {
        let stored_outflow = matches!(item.source, LineSource::PlannedItem { .. })
            && item.direction == PlanDirection::Outflow;
        let (true, Some(category)) = (stored_outflow, item.category) else {
            continue;
        };
        let figures = pools.entry((category, item.amount.currency)).or_default();
        let earlier = dated(item, month_start, input.as_of)?;
        figures.earlier += earlier;
        if matches!(item.schedule, PlanSchedule::Recurring { .. }) {
            if item.spending == Some(PlanSpending::Budget) {
                figures.remaining_budgets += dated(item, input.as_of, month_end)?;
            } else {
                figures.earlier_bills += earlier;
            }
        }
    }
    for (&(category, code), figures) in &pools {
        let lines = || {
            forecast
                .items
                .iter()
                .filter(move |line| line.category == Some(category) && line.gross.currency == code)
        };
        let reduced: i128 = lines().map(|line| wide(line.reduced_by)).sum();
        let pool = (figures.spent - figures.earlier).max(0);
        if reduced != pool.min(figures.remaining_budgets) {
            return Err("a pool was not spent exactly once".to_owned());
        }
        let unpaid: i128 = lines().map(|line| wide(line.unpaid)).sum();
        if unpaid != (figures.earlier_bills - figures.spent).max(0) {
            return Err("a bill's gap is wrong".to_owned());
        }
        let warned: i128 = forecast
            .cautions
            .iter()
            .filter_map(|caution| match caution {
                Caution::BillNotSeen {
                    category: found,
                    amount,
                } if *found == category && amount.currency == code => Some(wide(*amount)),
                _ => None,
            })
            .sum();
        if warned != unpaid {
            return Err("a gap was not reported in full".to_owned());
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn totals_explain_themselves(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        for row in &forecast.totals {
            let needed = wide(row.planned_outflows) + wide(row.goals) + wide(row.floor)
                - wide(row.planned_inflows);
            prop_assert_eq!(wide(row.cash_needed), needed, "needed adds up");
            if let Some(surplus) = row.surplus {
                prop_assert_eq!(
                    wide(surplus),
                    wide(row.cash_available) - needed,
                    "surplus is available less needed"
                );
            }

            let counted_cash: i128 = forecast
                .accounts
                .iter()
                .filter(|line| line.counted)
                .filter_map(|line| line.balance)
                .filter(|balance| balance.currency == row.currency)
                .map(wide)
                .sum();
            prop_assert_eq!(wide(row.cash_available), counted_cash, "cash adds up");

            let by_type = |wanted: PlanItemType, direction: PlanDirection| -> i128 {
                in_currency(&forecast, row.currency)
                    .filter(|line| line.item_type == wanted && line.direction == direction)
                    .map(|line| wide(line.counted))
                    .sum()
            };
            let out = PlanDirection::Outflow;
            let inn = PlanDirection::Inflow;
            prop_assert_eq!(
                wide(row.planned_outflows),
                by_type(PlanItemType::Recurring, out) + by_type(PlanItemType::OneOff, out),
                "item lines add up to the outflows"
            );
            prop_assert_eq!(
                wide(row.goals),
                by_type(PlanItemType::Goal, out),
                "item lines add up to the goals"
            );
            prop_assert_eq!(
                wide(row.planned_inflows),
                by_type(PlanItemType::Recurring, inn) + by_type(PlanItemType::OneOff, inn),
                "item lines add up to the inflows"
            );

            let lines: Vec<_> = forecast
                .months
                .iter()
                .filter_map(|bucket| {
                    bucket.lines.iter().find(|line| line.currency == row.currency)
                })
                .collect();
            prop_assert_eq!(lines.len(), forecast.months.len(), "a line in every month");
            let outflows: i128 = lines.iter().map(|line| wide(line.outflows)).sum();
            let accrual: i128 = lines.iter().map(|line| wide(line.goal_accrual)).sum();
            let inflows: i128 = lines.iter().map(|line| wide(line.inflows)).sum();
            prop_assert_eq!(outflows, wide(row.planned_outflows), "months add up to outflows");
            prop_assert_eq!(accrual, wide(row.goals), "months add up to goals");
            prop_assert_eq!(
                inflows,
                wide(row.planned_inflows) + wide(row.excluded_inflows),
                "months list all planned income"
            );

            let mut running = wide(row.cash_available);
            for line in &lines {
                let counted_in = if input.include_income { wide(line.inflows) } else { 0 };
                running += counted_in - wide(line.outflows);
                prop_assert_eq!(wide(line.projected_cash), running, "projected cash carries");
                let itemized: i128 = line.items.iter().map(|item| wide(item.amount)).sum();
                prop_assert_eq!(
                    itemized,
                    wide(line.outflows) + wide(line.inflows) + wide(line.goal_accrual),
                    "items explain the month"
                );
                prop_assert!(line.goal_accrual.minor_units >= 0, "accrual never falls");
                prop_assert!(
                    line.items.iter().all(|item| item.amount.minor_units > 0),
                    "item amounts are positive magnitudes"
                );
            }
        }
        for line in &forecast.items {
            prop_assert!(
                line.gross.minor_units != 0 || line.unpaid.minor_units != 0,
                "no zero lines"
            );
            prop_assert_eq!(
                line.counted.minor_units + line.reduced_by.minor_units,
                if line.direction == PlanDirection::Inflow && !input.include_income {
                    0
                } else {
                    line.gross.minor_units + line.unpaid.minor_units
                },
                "gross less reduction plus the unpaid gap is counted"
            );
            prop_assert!(line.counted.minor_units >= 0, "never negative");
            prop_assert!(line.reduced_by.minor_units >= 0, "never negative");
        }
        prop_assert_eq!(forecast.accounts.len(), input.accounts.len(), "one per account");
    }

    #[test]
    fn reductions_spend_each_pool_once(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        prop_assert_eq!(check_reduction(&input, &forecast), Ok(()), "reduction oracle");
    }

    #[test]
    fn windows_lose_no_day(as_of in as_of(), horizon_months in 1_u8..=60) {
        let forecast = compute_forecast(&ForecastInput {
            today: as_of,
            as_of,
            horizon_months,
            ..household()
        })
        .unwrap();
        let end = window_end(as_of, horizon_months).unwrap();
        prop_assert_eq!(forecast.end_exclusive, end, "the window's end");
        prop_assert_eq!(
            forecast.months.first().map(|bucket| bucket.month),
            CalendarMonth::of(as_of).ok(),
            "the first bucket holds as_of"
        );
        prop_assert_eq!(
            forecast.months.last().map(|bucket| bucket.month),
            CalendarMonth::of(end.yesterday().unwrap()).ok(),
            "the last bucket holds the last day"
        );
        for pair in forecast.months.windows(2) {
            let [earlier, later] = pair else {
                panic!("windows of two");
            };
            prop_assert_eq!(earlier.month.next(), later.month, "no month is skipped");
        }
    }

    #[test]
    fn status_matches_lists(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        let expected = if !forecast.blockers.is_empty() {
            ForecastStatus::Unavailable
        } else if forecast.cautions.is_empty() {
            ForecastStatus::Firm
        } else {
            ForecastStatus::Provisional
        };
        prop_assert_eq!(forecast.status, expected, "status follows the lists");
    }

    #[test]
    fn surplus_absent_iff_blocked(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        prop_assert_eq!(
            forecast.surplus.is_none(),
            !forecast.blockers.is_empty(),
            "a number exactly when nothing blocks"
        );
        if let Some(surplus) = forecast.surplus {
            let [row] = forecast.totals.as_slice() else {
                panic!("a number means exactly one currency");
            };
            prop_assert_eq!(row.surplus, Some(surplus), "the only currency's surplus");
        }
        let other_blocker = forecast
            .blockers
            .iter()
            .any(|blocker| !matches!(blocker, Blocker::MixedCurrencies { .. }));
        for row in &forecast.totals {
            prop_assert_eq!(
                row.surplus.is_none(),
                other_blocker,
                "a currency's surplus goes only with a blocker other than mixed"
            );
        }
        prop_assert_eq!(
            forecast.blockers.iter().any(|b| matches!(b, Blocker::MixedCurrencies { .. })),
            forecast.totals.len() > 1,
            "more than one row is exactly what mixed means"
        );
    }
}
