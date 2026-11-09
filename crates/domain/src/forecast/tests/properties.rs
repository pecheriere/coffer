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
    ForecastStatus, ItemInput, LooseEnds, SpentTransaction, compute_forecast, payments_read_from,
};
use crate::ids::{CategoryId, TransactionId};
use crate::money::{Currency, Money};
use crate::plan::{
    CalendarMonth, EmergencyFundTarget, EmergencyFunds, IntervalUnit, PlanDirection, PlanItemType,
    PlanSchedule, PlanTier, SettlementKind,
};

mod oracle;

use oracle::{check_reduction, check_settled_list, dated, unsettled};

pub(super) const MAX_AMOUNT: i64 = 1_000_000_000;

fn day(offset: i32) -> Date {
    AS_OF.checked_add(offset.days()).unwrap()
}

/// Mostly dollars, so most households have a single number to check.
pub(super) fn money(amount: impl Strategy<Value = i64>) -> impl Strategy<Value = Money> {
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

/// A few months around the ones `as_of` picks from, so that they often hold an occurrence
/// and the window often reaches them.
pub(super) fn settled_months() -> impl Strategy<Value = BTreeMap<CalendarMonth, SettlementKind>> {
    collection::btree_map(
        (-8..16).prop_map(|offset: i32| {
            CalendarMonth::of(
                date(2026, 11, 1)
                    .checked_add(i64::from(offset).months())
                    .unwrap(),
            )
            .unwrap()
        }),
        prop_oneof![Just(SettlementKind::Paid), Just(SettlementKind::Skipped)],
        0..5,
    )
}

/// `categories` is the pool items draw on: few, so that they share.
fn item(categories: Vec<CategoryId>) -> impl Strategy<Value = ItemInput> {
    (
        money(1..MAX_AMOUNT),
        schedule(),
        0..categories.len(),
        0..6,
        0..MAX_AMOUNT,
        settled_months(),
        0..3,
    )
        .prop_map(
            move |(amount, schedule, category, kind, spent, settled, tier)| {
                let is_goal = matches!(schedule, PlanSchedule::Goal { .. });
                let base = ItemInput {
                    category: categories.get(category).copied(),
                    goal_spent_minor: if is_goal { spent } else { 0 },
                    ..stored("Item", amount, schedule)
                };
                let is_recurring = matches!(base.schedule, PlanSchedule::Recurring { .. });
                let item = match kind {
                    0 if !is_goal => inflow(base),
                    1 => hypothetical(0, base),
                    2 | 3 if is_recurring => budget(base),
                    // Marked as a budget, a one-off or a goal stays what it is.
                    2 => budget(base),
                    // Only a stored bill or one-off going out may be settled.
                    _ if !is_goal => ItemInput { settled, ..base },
                    _ => base,
                };
                // Any outflow but a goal may sit in any of the three tiers, bills and budgets alike.
                if item.direction == PlanDirection::Outflow && !is_goal {
                    ItemInput {
                        tier: Some(match tier {
                            0 => PlanTier::MustPay,
                            1 => PlanTier::Essentials,
                            _ => PlanTier::Flexible,
                        }),
                        ..item
                    }
                } else {
                    item
                }
            },
        )
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

pub(super) fn spent_rows(
    picks: &[(usize, i64, i32, u8)],
    items: &[ItemInput],
    as_of: Date,
) -> Vec<SpentTransaction> {
    picks
        .iter()
        .filter_map(|&(pick, percent, days, kind)| {
            let item = items.get(pick)?;
            let minor = item.amount.minor_units * (100 + percent) / 100;
            Some(SpentTransaction {
                transaction: TransactionId::new(),
                category: item.category?,
                currency: item.amount.currency,
                posted_on: as_of
                    .checked_sub(days.days())
                    .unwrap()
                    .max(payments_read_from(as_of)),
                spent_minor: if kind == 0 { -minor } else { minor },
                possible_duplicate: kind == 1,
            })
        })
        .collect()
}

/// None, a fixed amount or some months.
fn target() -> impl Strategy<Value = Option<EmergencyFundTarget>> {
    prop_oneof![
        Just(None),
        (0..MAX_AMOUNT).prop_map(|minor_units| Some(EmergencyFundTarget::Fixed { minor_units })),
        (1_u8..13).prop_map(|months| Some(EmergencyFundTarget::MonthsOfOutflows { months })),
    ]
}

/// A target, or none, for each of dollars and euros.
pub(super) fn emergency_funds() -> impl Strategy<Value = EmergencyFunds> {
    (target(), target()).prop_map(|(dollars, euros)| {
        [("USD", dollars), ("EUR", euros)]
            .into_iter()
            .filter_map(|(code, target)| target.map(|target| (currency(code), target)))
            .collect()
    })
}

pub(super) fn input() -> impl Strategy<Value = ForecastInput> {
    let categories = vec![CategoryId::new(), CategoryId::new(), CategoryId::new()];
    let spent = collection::vec(
        (0..categories.len(), -1_000..MAX_AMOUNT, any::<bool>()),
        0..4,
    );
    (
        collection::vec(account(), 0..5),
        collection::vec(item(categories.clone()), 0..12),
        spent,
        emergency_funds(),
        1_u8..16,
        any::<bool>(),
        -2..2,
        collection::vec((0_u32..3, 0_u32..3, 0..MAX_AMOUNT), 0..2),
        (
            as_of(),
            collection::vec((0_usize..12, -15_i64..15, 0_i32..70, 0_u8..6), 0..12),
        ),
    )
        .prop_map(
            move |(
                accounts,
                items,
                spent,
                emergency_funds,
                horizon_months,
                include_income,
                shift,
                loose,
                (as_of, picks),
            )| {
                ForecastInput {
                    today: as_of.checked_add(i64::from(shift).days()).unwrap(),
                    as_of,
                    accounts,
                    emergency_funds,
                    horizon_months,
                    include_income,
                    spent_transactions: spent_rows(&picks, &items, as_of),
                    // Now and then, history that starts inside the look-back.
                    earliest_transaction: Some(if picks.len() % 4 == 0 {
                        as_of.checked_sub(20.days()).unwrap()
                    } else {
                        date(2020, 1, 1)
                    }),
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
                    items,
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

proptest! {
    #[test]
    fn totals_explain_themselves(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        for row in &forecast.totals {
            let needed = wide(row.planned_outflows) + wide(row.goals) + wide(row.emergency_fund)
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

            let income_end = input
                .as_of
                .checked_add(i64::from(input.horizon_months).months())
                .unwrap();
            let mut expected_inflows = 0;
            for item in &input.items {
                if item.direction == PlanDirection::Inflow && item.amount.currency == row.currency
                {
                    let from = input.as_of.tomorrow().unwrap();
                    expected_inflows += dated(item, from, income_end).unwrap();
                }
            }
            prop_assert_eq!(inflows, expected_inflows, "income counts after as_of, and the clamped day adds none");

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
                line.gross.minor_units != 0
                    || line.unpaid.minor_units != 0
                    || !line.payments.is_empty(),
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
    fn the_settled_list_says_what_was_taken_out(input in input()) {
        let forecast = compute_forecast(&input).unwrap();
        prop_assert_eq!(check_settled_list(&input, &forecast), Ok(()), "settled oracle");
    }

    /// A settled occurrence owes nothing, so spending moves on to other bills and budgets, and
    /// what it keeps off a budget is no more than settling saved: the need can only fall,
    /// whatever the plan.
    #[test]
    fn settling_never_raises_the_need_nor_touches_cash_or_the_emergency_fund(input in input()) {
        let settled = compute_forecast(&input).unwrap();
        let plain = compute_forecast(&unsettled(&input)).unwrap();
        for row in &settled.totals {
            let before = plain.totals.iter().find(|other| other.currency == row.currency);
            let Some(before) = before else {
                panic!("settling never adds a currency");
            };
            prop_assert!(row.cash_needed.minor_units <= before.cash_needed.minor_units, "need");
            prop_assert_eq!(row.cash_available, before.cash_available, "cash");
            prop_assert_eq!(row.emergency_fund, before.emergency_fund, "emergency fund");
        }
        let extra = settled.cautions.iter().filter(|caution| !plain.cautions.contains(caution));
        for caution in extra {
            // What a caution says can change with the need: a gap's amount, as spending moves
            // to another bill, which may then be unclear, how much history is missing, and the
            // first month cash dips below the emergency fund.
            prop_assert!(
                matches!(
                    caution,
                    Caution::BillNotSeen { .. }
                        | Caution::BillPaymentUnclear { .. }
                        | Caution::BillHistoryMissing { .. }
                        | Caution::RefundBesidePaidBill { .. }
                        | Caution::CashDipsBelowEmergencyFund { .. }
                ),
                "settling adds no caution of its own"
            );
        }
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
