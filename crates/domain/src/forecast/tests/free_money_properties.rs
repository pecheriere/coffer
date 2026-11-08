//! Random households: free money must explain itself, never mix currencies, and agree with the
//! forecast where the two look at the same days.

use std::collections::BTreeMap;

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, any, prop_assert, prop_assert_eq, prop_oneof};
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use proptest::{collection, proptest};

use super::properties::{MAX_AMOUNT, emergency_funds, input, money, settled_months, spent_rows};
use super::{budget, cash, currency, every, household, inflow, stored};
use crate::forecast::{
    Blocker, CategorySpent, ForecastInput, ForecastStatus, FreeMoney, ItemInput, compute_forecast,
    compute_free_money,
};
use crate::ids::CategoryId;
use crate::money::{Currency, Money};
use crate::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanTier};

fn wide(money: Money) -> i128 {
    i128::from(money.minor_units)
}

/// Whether a blocker in `answer` leaves `currency` without a number.
fn blocked(answer: &FreeMoney, input: &ForecastInput, currency: Currency) -> bool {
    answer.blockers.iter().any(|blocker| match blocker {
        Blocker::NoCountedAccounts => true,
        Blocker::BalanceMissing { account, .. } => input
            .accounts
            .iter()
            .any(|found| found.id == *account && found.currency == currency),
        Blocker::MixedCurrencies { .. } => false,
    })
}

/// `input` with only what is in dollars.
fn dollars_only(input: &ForecastInput) -> ForecastInput {
    let dollars = currency("USD");
    ForecastInput {
        accounts: input
            .accounts
            .iter()
            .filter(|account| account.currency == dollars)
            .cloned()
            .collect(),
        items: input
            .items
            .iter()
            .filter(|item| item.amount.currency == dollars)
            .cloned()
            .collect(),
        category_spent: input
            .category_spent
            .iter()
            .filter(|row| row.currency == dollars)
            .copied()
            .collect(),
        spent_transactions: input
            .spent_transactions
            .iter()
            .filter(|row| row.currency == dollars)
            .copied()
            .collect(),
        ..input.clone()
    }
}

/// The first day of a month of 31 days, where the forecast's one-month window and free money's
/// both end on the next month's first day.
fn first_of_a_long_month() -> impl Strategy<Value = Date> {
    prop_oneof![
        Just(date(2026, 12, 1)),
        Just(date(2027, 1, 1)),
        Just(date(2027, 3, 1)),
        Just(date(2027, 5, 1)),
        Just(date(2027, 8, 1)),
    ]
}

/// Bills, one-offs and budgets in every tier, and income, dated around `as_of` so that last
/// month's, this month's and next week's occurrences are all common. No goals: free money takes
/// what they set aside so far, the forecast what they need by the window's end.
fn dated_item(categories: Vec<CategoryId>, as_of: Date) -> impl Strategy<Value = ItemInput> {
    let day = move |offset: i32| as_of.checked_add(offset.days()).unwrap();
    let schedule = prop_oneof![
        (1_u16..4, 0..4, -400..40).prop_map(move |(step, unit, offset)| {
            let unit = match unit {
                0 => IntervalUnit::Days,
                1 => IntervalUnit::Weeks,
                2 => IntervalUnit::Months,
                _ => IntervalUnit::Years,
            };
            every(step, unit, day(offset))
        }),
        (-45..45).prop_map(move |offset| PlanSchedule::OneOff { on: day(offset) }),
        // Due in the first week, where a payment already made may have paid it.
        (0..8).prop_map(move |offset| every(1, IntervalUnit::Months, day(offset))),
    ];
    (
        money(1..MAX_AMOUNT),
        schedule,
        0..categories.len(),
        0..6,
        0..3,
        settled_months(),
    )
        .prop_map(move |(amount, schedule, category, kind, tier, settled)| {
            let base = ItemInput {
                category: categories.get(category).copied(),
                ..stored("Item", amount, schedule)
            };
            let tier = match tier {
                0 => PlanTier::MustPay,
                1 => PlanTier::Essentials,
                _ => PlanTier::Flexible,
            };
            let recurring = matches!(base.schedule, PlanSchedule::Recurring { .. });
            match kind {
                0 => inflow(base),
                1 | 2 if recurring => ItemInput {
                    tier: Some(tier),
                    ..budget(base)
                },
                _ => ItemInput {
                    tier: Some(tier),
                    settled,
                    ..base
                },
            }
        })
}

/// A household as of the first of a long month: dollars and euros, payments of last month's
/// bills, this month's and early ones, and the month's spending made of them.
fn first_of_month_input() -> impl Strategy<Value = ForecastInput> {
    let categories = vec![CategoryId::new(), CategoryId::new(), CategoryId::new()];
    first_of_a_long_month().prop_flat_map(move |as_of| {
        let picks = collection::vec(
            (
                0_usize..10,
                -15_i64..15,
                prop_oneof![0_i32..8, 0_i32..70],
                0_u8..6,
            ),
            0..12,
        );
        (
            collection::vec(dated_item(categories.clone(), as_of), 0..10),
            picks,
            emergency_funds(),
            any::<bool>(),
            collection::vec(money(-MAX_AMOUNT..MAX_AMOUNT), 1..3),
        )
            .prop_map(
                move |(items, picks, emergency_funds, include_income, balances)| {
                    let spent_transactions = spent_rows(&picks, &items, as_of);
                    let mut spending: BTreeMap<(CategoryId, Currency), i64> = BTreeMap::new();
                    for row in spent_transactions
                        .iter()
                        .filter(|row| row.posted_on >= as_of.first_of_month())
                    {
                        *spending.entry((row.category, row.currency)).or_default() +=
                            row.spent_minor;
                    }
                    ForecastInput {
                        today: as_of,
                        as_of,
                        horizon_months: 1,
                        include_income,
                        emergency_funds,
                        accounts: balances
                            .into_iter()
                            .map(|balance| cash("Checking", balance))
                            .collect(),
                        category_spent: spending
                            .into_iter()
                            .map(|((category, currency), spent_minor)| CategorySpent {
                                category,
                                currency,
                                spent_minor,
                            })
                            .collect(),
                        spent_transactions,
                        items,
                        ..household()
                    }
                },
            )
    })
}

proptest! {
    #[test]
    fn free_money_explains_itself(input in input()) {
        let input = ForecastInput { today: input.as_of, ..input };
        let answer = compute_free_money(&input).unwrap();
        let forecast = compute_forecast(&ForecastInput {
            horizon_months: 1,
            include_income: false,
            ..input.clone()
        })
        .unwrap();

        for pair in answer.currencies.windows(2) {
            let [earlier, later] = pair else { panic!("windows of two") };
            prop_assert!(earlier.currency < later.currency, "sorted, one row each");
        }
        prop_assert!(
            !answer.blockers.iter().any(|b| matches!(b, Blocker::MixedCurrencies { .. })),
            "free money is per currency"
        );
        for row in &answer.currencies {
            let need = row.still_need;
            let parts = [need.must_pay_this_month, need.must_pay_coming, need.essentials, need.flexible];
            prop_assert!(parts.iter().all(|part| part.minor_units >= 0), "no part below 0");
            prop_assert_eq!(wide(need.total), parts.into_iter().map(wide).sum::<i128>(), "parts add up");
            prop_assert!(row.set_aside.minor_units >= 0, "set aside is never below 0");
            prop_assert!(row.emergency_fund_target.minor_units >= 0, "nor the target");
            prop_assert_eq!(row.emergency_fund.is_none(), !input.emergency_funds.contains_key(&row.currency));
            let forecast_cash = forecast
                .totals
                .iter()
                .find(|totals| totals.currency == row.currency)
                .map_or(0, |totals| wide(totals.cash_available));
            prop_assert_eq!(wide(row.cash_available), forecast_cash, "the forecast's cash");

            prop_assert_eq!(row.free_money.is_none(), blocked(&answer, &input, row.currency), "blocked");
            prop_assert_eq!(row.free_money.is_none(), row.emergency_fund_held.is_none());
            prop_assert_eq!(row.free_money.is_none(), row.shortfall.is_none());
            let (Some(free), Some(held), Some(shortfall)) =
                (row.free_money, row.emergency_fund_held, row.shortfall)
            else {
                continue;
            };
            let before = wide(row.cash_available) - wide(need.total) - wide(row.set_aside);
            let target = wide(row.emergency_fund_target);
            prop_assert_eq!(wide(free) + wide(held) - wide(shortfall), before, "it adds up");
            prop_assert!(free.minor_units >= 0 && shortfall.minor_units >= 0, "never below 0");
            prop_assert!((0..=target).contains(&wide(held)), "held is within the target");
            if free.minor_units > 0 {
                prop_assert_eq!(wide(held), target, "free only once the fund is whole");
                prop_assert_eq!(shortfall.minor_units, 0);
            }
            if shortfall.minor_units > 0 {
                prop_assert_eq!(held.minor_units, 0, "a shortfall holds no fund");
                prop_assert_eq!(free.minor_units, 0, "and frees nothing");
            }
        }

        let expected = if answer.currencies.iter().all(|row| row.free_money.is_none()) {
            ForecastStatus::Unavailable
        } else if answer.blockers.is_empty() && answer.cautions.is_empty() {
            ForecastStatus::Firm
        } else {
            ForecastStatus::Provisional
        };
        prop_assert_eq!(answer.status, expected, "status follows the lists");
    }

    /// What is in euros changes nothing of dollars' numbers.
    #[test]
    fn currencies_never_mix(input in input()) {
        let input = ForecastInput { today: input.as_of, ..input };
        let numbers = |answer: &FreeMoney| {
            answer
                .currencies
                .iter()
                .find(|row| row.currency == currency("USD"))
                .map(|row| (row.cash_available, row.still_need, row.set_aside, row.emergency_fund_target))
        };
        let mixed = compute_free_money(&input).unwrap();
        let alone = compute_free_money(&dollars_only(&input)).unwrap();
        prop_assert_eq!(numbers(&mixed), numbers(&alone));
    }

    /// On the first of a 31-day month the must pay window is the forecast's one-month window,
    /// and this month is all of it, so what the plan still needs is the forecast's outflows.
    #[test]
    fn on_the_first_still_need_is_the_forecasts_outflows(input in first_of_month_input()) {
        let answer = compute_free_money(&input).unwrap();
        let forecast = compute_forecast(&input).unwrap();
        let ours: Vec<(Currency, Money)> = answer
            .currencies
            .iter()
            .map(|row| (row.currency, row.still_need.total))
            .collect();
        let theirs: Vec<(Currency, Money)> = forecast
            .totals
            .iter()
            .map(|row| (row.currency, row.planned_outflows))
            .collect();
        prop_assert_eq!(ours, theirs);
    }
}

/// The generator behind the last property makes the cases it is there for: bills paid ahead,
/// bills unpaid from before today, budgets reduced, months settled, income and euros.
#[test]
fn first_of_month_households_cover_the_cases() {
    let mut runner = TestRunner::deterministic();
    let strategy = first_of_month_input();
    let (mut paid_ahead, mut unpaid, mut reduced, mut settled, mut income, mut euros) =
        (0, 0, 0, 0, 0, 0);
    for _ in 0..512 {
        let input = strategy.new_tree(&mut runner).unwrap().current();
        let forecast = compute_forecast(&input).unwrap();
        for line in &forecast.items {
            paid_ahead += usize::from(
                line.payments
                    .iter()
                    .any(|payment| payment.on >= input.as_of && payment.paid_by.is_some()),
            );
            unpaid += usize::from(line.unpaid.minor_units > 0);
            reduced += usize::from(line.reduced_by.minor_units > 0);
        }
        settled += usize::from(!forecast.settled.is_empty());
        income += usize::from(
            input
                .items
                .iter()
                .any(|item| item.direction == PlanDirection::Inflow),
        );
        euros += usize::from(forecast.totals.len() > 1);
    }
    for (name, count) in [
        ("paid ahead", paid_ahead),
        ("unpaid", unpaid),
        ("reduced", reduced),
        ("settled", settled),
        ("income", income),
        ("two currencies", euros),
    ] {
        assert!(count >= 10, "{name}: {count} of 512");
    }
}
