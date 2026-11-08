//! R9a: settling a bill must never make a household hold more. Each case is worked by hand from
//! the dates and amounts here, with the need before and after. Then random households built to
//! crowd one category, where a payment fits more than one bill and a budget has room to lose.

use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, Strategy, any, prop_assert, prop_oneof};
use proptest::{collection, proptest};

use super::{
    budget, every, fixed_fund, household, monthly, one_off, paid_on, run_as_given, spent, stored,
    usd, usd_totals,
};
use crate::forecast::{Caution, ForecastInput, SpentTransaction};
use crate::ids::CategoryId;
use crate::plan::{
    CalendarMonth, EmergencyFundTarget, EmergencyFunds, IntervalUnit, PlanSchedule, SettlementKind,
};

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

fn need(input: &ForecastInput) -> i64 {
    usd_totals(&run_as_given(input)).cash_needed.minor_units
}

/// The same household with nothing settled.
fn unsettled(input: &ForecastInput) -> ForecastInput {
    let mut clear = input.clone();
    for item in &mut clear.items {
        item.settled.clear();
    }
    clear
}

/// As of 15 November with a month's window, to 15 December. History starts on 1 November, so 5
/// October is before anything anyone can see.
fn from_november(home: CategoryId, upkeep: i64) -> ForecastInput {
    let mut rent = monthly("Rent", usd(1_000), date(2026, 1, 5), home);
    rent.settled.insert(month("2026-10"), SettlementKind::Paid);
    ForecastInput {
        items: vec![
            rent,
            budget(monthly("Upkeep", usd(upkeep), date(2026, 11, 20), home)),
        ],
        horizon_months: 1,
        emergency_funds: fixed_fund(usd(0)),
        earliest_transaction: Some(date(2026, 11, 1)),
        ..household()
    }
}

/// As of 5 November, the day a monthly rent falls due, with a budget of 1,000 beside it. The
/// window ends on 5 December, so the rent counts once.
fn on_rent_day(home: CategoryId, spent_minor: i64, rows: Vec<SpentTransaction>) -> ForecastInput {
    let mut rent = monthly("Rent", usd(1_000), date(2026, 1, 5), home);
    rent.settled.insert(month("2026-10"), SettlementKind::Paid);
    ForecastInput {
        today: date(2026, 11, 5),
        as_of: date(2026, 11, 5),
        items: vec![
            rent,
            budget(monthly("Upkeep", usd(1_000), date(2026, 11, 20), home)),
        ],
        category_spent: vec![spent(home, spent_minor)],
        spent_transactions: rows,
        horizon_months: 1,
        emergency_funds: fixed_fund(usd(0)),
        ..household()
    }
}

/// The input proptest found, without what doesn't matter to it. As of 28 February 2027 to 28 March,
/// history starting on 8 February, so January is before it, and January settled as paid. A daily
/// bill and a daily budget: 55 of each in the window's reach, 1 February to 27 March, of which the
/// bill counts 27 unpaid in February, the 28th, and 27 in March, 55 × 1,000. The budget's 55 less
/// the 1 spent is 54. January owed nothing to begin with, so settling it changes nothing.
#[test]
fn settling_a_month_before_the_history_keeps_the_need_at_the_end_of_february() {
    let home = CategoryId::new();
    let daily = every(1, IntervalUnit::Days, date(2026, 11, 15));
    let mut bill = stored("Bill", usd(1_000), daily);
    bill.category = Some(home);
    bill.settled.insert(month("2027-01"), SettlementKind::Paid);
    let mut allowance = stored("Allowance", usd(1), daily);
    allowance.category = Some(home);
    let input = ForecastInput {
        today: date(2027, 2, 28),
        as_of: date(2027, 2, 28),
        horizon_months: 1,
        emergency_funds: EmergencyFunds::new(),
        accounts: Vec::new(),
        items: vec![bill, budget(allowance)],
        category_spent: vec![spent(home, 1)],
        earliest_transaction: Some(date(2027, 2, 8)),
        ..household()
    };
    assert_eq!(need(&unsettled(&input)), 55_054);
    assert_eq!(need(&input), 55_054);
}

/// October's rent, before the history, owed nothing, so settling it saves nothing and may take
/// nothing either. 5 November 1,000, 5 December 1,000, the budget's 500 less the 300 spent: 2,200.
#[test]
fn a_bill_settled_before_the_history_charges_no_budget() {
    let home = CategoryId::new();
    let input = ForecastInput {
        category_spent: vec![spent(home, 300)],
        spent_transactions: vec![paid_on(home, usd(300), date(2026, 11, 14))],
        ..from_november(home, 500)
    };
    assert_eq!(need(&unsettled(&input)), 2_200);
    assert_eq!(need(&input), 2_200);
}

/// 950 might be November's rent or the budget's spending, and is left to the budget, which needs
/// 50 more. A rent settled before the history doesn't take it: the need stays at 1,000 + 1,000 +
/// 50, and November's still says what might have paid it.
#[test]
fn a_bill_settled_before_the_history_takes_no_payment() {
    let home = CategoryId::new();
    let input = ForecastInput {
        category_spent: vec![spent(home, 950)],
        spent_transactions: vec![paid_on(home, usd(950), date(2026, 11, 6))],
        ..from_november(home, 1_000)
    };
    assert_eq!(need(&unsettled(&input)), 2_050);
    assert_eq!(need(&input), 2_050);
    let settled = run_as_given(&input);
    assert!(
        settled
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::BillPaymentUnclear { .. })),
        "the payment still points at November's rent"
    );
}

/// Unsettled, the 1,000 pays October and today's rent counts: 1,000, and the budget's 1,000 less
/// the 500 of spending left, 1,500. §10.8: settled, the payment goes on to today's rent, which then
/// needs nothing, so settling saved 1,000 and October's charge, which no transaction fits, may take
/// all 500 of the budget's spending: the budget's 1,000.
#[test]
fn a_payment_moving_to_todays_bill_pays_it() {
    let home = CategoryId::new();
    let input = on_rent_day(
        home,
        1_500,
        vec![
            paid_on(home, usd(500), date(2026, 11, 4)),
            paid_on(home, usd(1_000), date(2026, 11, 5)),
        ],
    );
    assert_eq!(need(&unsettled(&input)), 1_500);
    assert_eq!(need(&input), 1_000);
}

/// As above, with 950 more that can't be told from the budget's spending: unsettled, the rent's
/// 1,000 and the budget's 1,000 less 950, 1,050. §10.8: settled, the 1,000 pays today's rent, which
/// needs nothing, and the 950 fits October, so it is October's and no budget's: the budget's 1,000.
#[test]
fn a_payment_moving_to_todays_bill_takes_what_fits_october_off_the_budget() {
    let home = CategoryId::new();
    let input = on_rent_day(
        home,
        1_950,
        vec![
            paid_on(home, usd(1_000), date(2026, 11, 1)),
            paid_on(home, usd(950), date(2026, 11, 2)),
        ],
    );
    assert_eq!(need(&unsettled(&input)), 1_050);
    assert_eq!(need(&input), 1_000);
}

/// 110 is the exact amount of a bill on the 3rd, and fits one of 100 on the 5th too, in tolerance.
/// Before: it pays the 3rd, the 5th owes 100, and the 105 left of 215 clears the budget. Settle the
/// 3rd and it pays the 5th, saving 100, but the settled 110 may only cost the budget that 100: the
/// need stays at 100.
#[test]
fn a_settled_bill_costs_a_budget_no_more_than_settling_saved() {
    let home = CategoryId::new();
    let mut larger = one_off("Larger", usd(110), date(2026, 11, 3), home);
    larger
        .settled
        .insert(month("2026-11"), SettlementKind::Paid);
    let input = ForecastInput {
        items: vec![
            larger,
            one_off("Smaller", usd(100), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(105), date(2026, 11, 20), home)),
        ],
        category_spent: vec![spent(home, 215)],
        spent_transactions: vec![paid_on(home, usd(110), date(2026, 11, 6))],
        horizon_months: 1,
        emergency_funds: fixed_fund(usd(0)),
        ..household()
    };
    assert_eq!(need(&unsettled(&input)), 100);
    assert_eq!(need(&input), 100);
}

/// All of a charge that fits a settled bill is no budget's. Rent of 2,000, charged 2,100, and a
/// budget of 500: unsettled the charge pays the rent and the budget needs all 500, and settling
/// the rent, which says the same, changes nothing.
#[test]
fn a_settled_bill_keeps_all_of_a_charge_that_fits_off_the_budget() {
    let home = CategoryId::new();
    let mut rent = one_off("Rent", usd(2_000), date(2026, 11, 5), home);
    rent.settled.insert(month("2026-11"), SettlementKind::Paid);
    let input = ForecastInput {
        items: vec![
            rent,
            budget(monthly("Upkeep", usd(500), date(2026, 11, 20), home)),
        ],
        category_spent: vec![spent(home, 2_100)],
        spent_transactions: vec![paid_on(home, usd(2_100), date(2026, 11, 5))],
        horizon_months: 1,
        emergency_funds: fixed_fund(usd(0)),
        ..household()
    };
    assert_eq!(need(&unsettled(&input)), 500);
    assert_eq!(need(&input), 500);
}

/// The floor reads from what the budget could use, not all the pool held. 1,100 is the exact amount
/// of a bill on the 3rd and fits one of 1,000 on the 5th, and is more than the budget of 1,050
/// plans for November, so it is a bill's either way. Unsettled: it pays the 3rd, the 5th owes
/// 1,000, and the 1,100 left of 2,200 clears the budget: 1,000. Settle the 3rd: it pays the 5th,
/// saving 1,000, and the 3rd's 1,100, which nothing else fits, is charged, leaving the pool at 0.
/// The floor is the 1,050 the budget used less that 1,000, so the budget needs 1,000 and the need
/// stays at 1,000; the 1,100 the pool held would have left 950.
#[test]
fn the_floor_keeps_only_what_the_budget_could_use() {
    let home = CategoryId::new();
    let mut larger = one_off("Larger", usd(1_100), date(2026, 11, 3), home);
    larger
        .settled
        .insert(month("2026-11"), SettlementKind::Paid);
    let input = ForecastInput {
        items: vec![
            larger,
            one_off("Smaller", usd(1_000), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(1_050), date(2026, 11, 20), home)),
        ],
        category_spent: vec![spent(home, 2_200)],
        spent_transactions: vec![
            paid_on(home, usd(1_100), date(2026, 11, 3)),
            paid_on(home, usd(600), date(2026, 11, 8)),
            paid_on(home, usd(500), date(2026, 11, 9)),
        ],
        horizon_months: 1,
        emergency_funds: fixed_fund(usd(0)),
        ..household()
    };
    assert_eq!(need(&unsettled(&input)), 1_000);
    assert_eq!(need(&input), 1_000);
}

/// Dates where a month's length matters, an ordinary one, and a bill's own day.
fn crowded_as_of() -> impl Strategy<Value = Date> {
    prop_oneof![
        Just(date(2027, 2, 28)),
        Just(date(2028, 2, 29)),
        Just(date(2027, 1, 31)),
        Just(date(2027, 3, 1)),
        Just(date(2027, 3, 31)),
        Just(date(2026, 11, 5)),
        Just(date(2026, 11, 15)),
    ]
}

/// Amounts within tolerance of each other, so that a payment fits several.
fn crowded_amount() -> impl Strategy<Value = i64> {
    prop_oneof![Just(1_000), Just(950), Just(1_050), 900..1_100_i64]
}

/// Nearby dates, with February's short month in reach of a bill due on the 29th to the 31st.
fn crowded_schedule(as_of: Date) -> impl Strategy<Value = PlanSchedule> {
    let near = move |back: i32| as_of.checked_sub(back.days()).unwrap();
    prop_oneof![
        (0..70).prop_map(move |back| every(1, IntervalUnit::Months, near(back))),
        (0..21).prop_map(move |back| every(1, IntervalUnit::Weeks, near(back))),
        (-10..70).prop_map(move |back| PlanSchedule::OneOff { on: near(back) }),
        (29..=31_i8).prop_map(|day| every(1, IntervalUnit::Months, date(2026, 10, day))),
    ]
}

fn crowded_household() -> impl Strategy<Value = ForecastInput> {
    let home = CategoryId::new();
    crowded_as_of().prop_flat_map(move |as_of| {
        let bills = collection::vec(
            (
                crowded_amount(),
                crowded_schedule(as_of),
                0_i32..3,
                any::<bool>(),
            ),
            1..5,
        );
        let budgets = collection::vec((0_i64..2_500, 1_i8..=28), 0..3);
        let payments = collection::vec((crowded_amount(), 0_i32..45, 0_u8..8), 0..7);
        let history = prop_oneof![
            Just(date(2020, 1, 1)),
            (0_i32..50).prop_map(move |back| as_of.checked_sub(back.days()).unwrap()),
        ];
        (
            bills,
            budgets,
            payments,
            history,
            0_i64..6_000,
            1_u8..3,
            prop_oneof![
                Just(EmergencyFunds::new()),
                Just(fixed_fund(usd(0))),
                (1_u8..4).prop_map(|months| EmergencyFunds::from([(
                    usd(0).currency,
                    EmergencyFundTarget::MonthsOfOutflows { months },
                )])),
            ],
        )
            .prop_map(
                move |(bills, budgets, payments, history, spent_minor, horizon_months, funds)| {
                    let this_month = CalendarMonth::of(as_of).unwrap();
                    let last_month =
                        CalendarMonth::of(as_of.first_of_month().checked_sub(1.days()).unwrap())
                            .unwrap();
                    let mut items = Vec::new();
                    for (amount, schedule, which, paid) in bills {
                        let mut bill = stored("Bill", usd(amount), schedule);
                        bill.category = Some(home);
                        let kind = if paid {
                            SettlementKind::Paid
                        } else {
                            SettlementKind::Skipped
                        };
                        if which != 1 {
                            bill.settled.insert(last_month, kind);
                        }
                        if which != 0 {
                            bill.settled.insert(this_month, kind);
                        }
                        items.push(bill);
                    }
                    for (amount, day) in budgets {
                        let anchor = date(2026, 10, day);
                        items.push(budget(monthly("Upkeep", usd(amount), anchor, home)));
                    }
                    let rows = payments
                        .into_iter()
                        .map(|(amount, back, kind)| {
                            let on = as_of.checked_sub(back.days()).unwrap();
                            let mut row = paid_on(home, usd(amount), on);
                            if kind == 0 {
                                row.spent_minor = -row.spent_minor;
                            }
                            row.possible_duplicate = kind == 1;
                            row
                        })
                        .collect();
                    ForecastInput {
                        today: as_of,
                        as_of,
                        horizon_months,
                        emergency_funds: funds,
                        items,
                        category_spent: vec![spent(home, spent_minor)],
                        spent_transactions: rows,
                        earliest_transaction: Some(history),
                        ..household()
                    }
                },
            )
    })
}

proptest! {
    /// The general property draws a household like this too rarely: a settled bill beside a
    /// budget with spending, a payment that fits two bills, and history starting late.
    #[test]
    fn settling_never_raises_the_need_in_a_crowded_category(input in crowded_household()) {
        prop_assert!(
            need(&input) <= need(&unsettled(&input)),
            "settling raised the need"
        );
    }
}
