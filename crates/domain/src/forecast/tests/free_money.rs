//! Free money (money redesign §4.3, §4.4, §10.1), worked by hand from the dates and amounts
//! here. Most cases are as of 15 January 2027, in dollars, with checking holding 10,000.00.

use std::collections::BTreeMap;

use jiff::civil::{Date, date};

use super::{
    budget, cash, currency, eur, fixed_fund, goal, household, inflow, monthly, one_off, paid_on,
    spent, usd, with_spending,
};
use crate::accounts::LiquidityClass;
use crate::forecast::{
    AccountInput, BalanceInput, Blocker, Caution, ForecastInput, ForecastStatus, FreeMoney,
    FreeMoneyCurrency, ItemInput, compute_free_money,
};
use crate::ids::CategoryId;
use crate::money::Money;
use crate::plan::{
    CalendarMonth, EmergencyFundTarget, EmergencyFunds, PlanSchedule, PlanTier, SettlementKind,
};

const JAN_15: Date = date(2027, 1, 15);

/// Checking holding `checking` as of the day before `today`, a target of 3,000.00 and nothing
/// planned.
fn on(today: Date, checking: Money) -> ForecastInput {
    ForecastInput {
        today,
        as_of: today,
        emergency_funds: fixed_fund(usd(300_000)),
        accounts: vec![held("Checking", checking, today)],
        ..household()
    }
}

/// A counted cash account holding `balance` as of the day before `today`.
fn held(name: &str, balance: Money, today: Date) -> AccountInput {
    AccountInput {
        balance: Some(BalanceInput {
            on: today.yesterday().unwrap(),
            minor_units: balance.minor_units,
        }),
        ..cash(name, balance)
    }
}

/// Free money for `input`, last month's bills paid on their dates and each category's spending
/// one transaction, as the forecast's tests mean it.
fn free(input: &ForecastInput) -> FreeMoney {
    compute_free_money(&with_spending(input)).unwrap()
}

fn dollars(answer: &FreeMoney) -> &FreeMoneyCurrency {
    answer
        .currencies
        .iter()
        .find(|row| row.currency == currency("USD"))
        .unwrap()
}

fn tiered(item: ItemInput, tier: PlanTier) -> ItemInput {
    ItemInput {
        tier: Some(tier),
        ..item
    }
}

fn rent(category: CategoryId) -> ItemInput {
    monthly("Rent", usd(200_000), date(2027, 1, 20), category)
}

fn groceries(category: CategoryId) -> ItemInput {
    tiered(
        budget(monthly(
            "Groceries",
            usd(60_000),
            date(2027, 1, 1),
            category,
        )),
        PlanTier::Essentials,
    )
}

fn restaurants(category: CategoryId) -> ItemInput {
    budget(monthly(
        "Restaurants",
        usd(30_000),
        date(2027, 1, 1),
        category,
    ))
}

#[test]
fn the_formula_in_order() {
    let (home, food, out) = (CategoryId::new(), CategoryId::new(), CategoryId::new());
    let answer = free(&ForecastInput {
        items: vec![
            rent(home),
            groceries(food),
            restaurants(out),
            // 184 of its 365 days gone: exactly half of 736 days' worth, 368.00.
            goal("Trip", usd(73_000), date(2026, 7, 15), date(2027, 7, 15), 0),
        ],
        category_spent: vec![spent(food, 25_000), spent(out, 10_000)],
        ..on(JAN_15, usd(1_000_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.cash_available, usd(1_000_000));
    assert_eq!(row.still_need.must_pay_this_month, usd(200_000), "rent");
    assert_eq!(
        row.still_need.must_pay_coming,
        usd(0),
        "February's is past the window"
    );
    assert_eq!(row.still_need.essentials, usd(35_000), "600 less 250");
    assert_eq!(row.still_need.flexible, usd(20_000), "300 less 100");
    assert_eq!(row.still_need.total, usd(255_000));
    assert_eq!(row.set_aside, usd(36_800));
    assert_eq!(
        row.emergency_fund,
        Some(EmergencyFundTarget::Fixed {
            minor_units: 300_000
        })
    );
    assert_eq!(row.emergency_fund_target, usd(300_000));
    assert_eq!(row.emergency_fund_held, Some(usd(300_000)), "fully held");
    assert_eq!(row.free_money, Some(usd(408_200)), "7,082 less 3,000");
    assert_eq!(row.shortfall, Some(usd(0)));
    assert_eq!(answer.as_of, JAN_15);
    assert_eq!(answer.must_pay_through, date(2027, 2, 14));
    assert_eq!(answer.status, ForecastStatus::Firm);
    assert_eq!(answer.blockers, []);
    assert_eq!(answer.cautions, []);
}

/// The mortgage of 1 February, paid for January on its date.
fn mortgage_on_the_30th(home: CategoryId, more: &[(Date, i64)]) -> FreeMoney {
    let today = date(2027, 1, 30);
    let mut rows = vec![paid_on(home, usd(150_000), date(2027, 1, 1))];
    rows.extend(
        more.iter()
            .map(|&(on, minor)| paid_on(home, usd(minor), on)),
    );
    free(&ForecastInput {
        items: vec![monthly("Mortgage", usd(150_000), date(2026, 12, 1), home)],
        spent_transactions: rows,
        ..on(today, usd(1_000_000))
    })
}

/// Thirty days from 30 January is 1 March, so both February's and March's mortgage count.
#[test]
fn a_must_pay_due_next_month_is_still_needed() {
    let answer = mortgage_on_the_30th(CategoryId::new(), &[]);
    assert_eq!(answer.must_pay_through, date(2027, 3, 1));
    let row = dollars(&answer);
    assert_eq!(
        row.still_need.must_pay_this_month,
        usd(0),
        "January's is paid"
    );
    assert_eq!(
        row.still_need.must_pay_coming,
        usd(300_000),
        "1 February's and 1 March's aren't free"
    );
    assert_eq!(row.free_money, Some(usd(1_000_000 - 300_000 - 300_000)));
}

#[test]
fn a_bill_paid_early_is_not_needed() {
    let answer = mortgage_on_the_30th(CategoryId::new(), &[(date(2027, 1, 29), 150_000)]);
    let row = dollars(&answer);
    assert_eq!(
        row.still_need.must_pay_coming,
        usd(150_000),
        "February's cash is already gone, March's is still needed"
    );
    assert_eq!(row.free_money, Some(usd(1_000_000 - 150_000 - 300_000)));
}

#[test]
fn the_window_ends_on_the_thirtieth_day() {
    let today = date(2027, 1, 10);
    let other = CategoryId::new();
    let answer = free(&ForecastInput {
        items: vec![
            one_off("In", usd(1_000), date(2027, 2, 9), other),
            one_off("Out", usd(2_000), date(2027, 2, 10), other),
        ],
        ..on(today, usd(1_000_000))
    });
    assert_eq!(answer.must_pay_through, date(2027, 2, 9));
    let row = dollars(&answer);
    assert_eq!(
        row.still_need.must_pay_coming,
        usd(1_000),
        "the thirtieth day counts"
    );
    assert_eq!(row.still_need.total, usd(1_000), "the thirty-first doesn't");
}

#[test]
fn a_bill_settled_or_paid_this_month_needs_nothing() {
    let (home, cover) = (CategoryId::new(), CategoryId::new());
    let january = CalendarMonth::of(JAN_15).unwrap();
    let answer = free(&ForecastInput {
        items: vec![
            ItemInput {
                settled: BTreeMap::from([(january, SettlementKind::Paid)]),
                ..rent(home)
            },
            monthly("Insurance", usd(10_000), date(2027, 1, 5), cover),
        ],
        // Within the 10% either way: the bill is paid, and the 8.00 more is simply gone.
        spent_transactions: vec![paid_on(cover, usd(10_800), date(2027, 1, 5))],
        ..on(JAN_15, usd(1_000_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.still_need.must_pay_this_month, usd(0), "both are paid");
    assert_eq!(
        row.still_need.must_pay_coming,
        usd(10_000),
        "February's insurance only: nothing added for the 8.00"
    );
    assert_eq!(row.free_money, Some(usd(690_000)));
}

#[test]
fn next_months_flexible_budget_is_not_needed() {
    let out = CategoryId::new();
    let answer = free(&ForecastInput {
        items: vec![restaurants(out)],
        category_spent: vec![spent(out, 10_000)],
        ..on(date(2027, 1, 30), usd(1_000_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.still_need.flexible, usd(20_000), "January's rest only");
    assert_eq!(row.still_need.total, usd(20_000));
}

#[test]
fn an_overspent_budget_needs_nothing_and_takes_nothing() {
    let (food, out) = (CategoryId::new(), CategoryId::new());
    let answer = free(&ForecastInput {
        items: vec![groceries(food), restaurants(out)],
        category_spent: vec![spent(food, 25_000), spent(out, 45_000)],
        ..on(JAN_15, usd(1_000_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.still_need.flexible, usd(0), "never below 0");
    assert_eq!(
        row.still_need.essentials,
        usd(35_000),
        "groceries' need is its own"
    );
    assert_eq!(row.still_need.total, usd(35_000));
}

#[test]
fn no_income_counts() {
    let pay = CategoryId::new();
    let without = on(JAN_15, usd(1_000_000));
    let with = ForecastInput {
        include_income: true,
        items: vec![inflow(one_off(
            "Salary",
            usd(500_000),
            date(2027, 1, 16),
            pay,
        ))],
        ..without.clone()
    };
    assert_eq!(dollars(&free(&with)), dollars(&free(&without)));
}

#[test]
fn no_target_counts_everything_as_free_with_a_caution() {
    let home = CategoryId::new();
    let answer = free(&ForecastInput {
        emergency_funds: EmergencyFunds::new(),
        items: vec![rent(home)],
        ..on(JAN_15, usd(1_000_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.emergency_fund, None);
    assert_eq!(row.emergency_fund_target, usd(0));
    assert_eq!(row.emergency_fund_held, Some(usd(0)));
    assert_eq!(row.free_money, Some(usd(800_000)), "all of before");
    assert_eq!(
        answer.cautions,
        [Caution::EmergencyFundNotSet {
            currency: currency("USD"),
        }]
    );
    assert_eq!(answer.status, ForecastStatus::Provisional);
}

#[test]
fn months_target_uses_must_pay_and_essentials() {
    let (home, food, out) = (CategoryId::new(), CategoryId::new(), CategoryId::new());
    let answer = free(&ForecastInput {
        emergency_funds: EmergencyFunds::from([(
            currency("USD"),
            EmergencyFundTarget::MonthsOfOutflows { months: 2 },
        )]),
        items: vec![rent(home), groceries(food), restaurants(out)],
        ..on(JAN_15, usd(1_000_000))
    });
    // To 15 March: rent on 20 January and 20 February, groceries on 1 February and 1 March.
    assert_eq!(dollars(&answer).emergency_fund_target, usd(520_000));
}

#[test]
fn shortfall_when_spoken_for_exceeds_cash() {
    let answer = free(&ForecastInput {
        items: vec![rent(CategoryId::new())],
        ..on(JAN_15, usd(100_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.free_money, Some(usd(0)));
    assert_eq!(row.emergency_fund_held, Some(usd(0)));
    assert_eq!(row.shortfall, Some(usd(100_000)));
}

/// Between 0 and the target, the cash holds part of the emergency fund and nothing is free.
#[test]
fn part_of_the_fund_is_held() {
    let answer = free(&ForecastInput {
        items: vec![rent(CategoryId::new())],
        ..on(JAN_15, usd(300_000))
    });
    let row = dollars(&answer);
    assert_eq!(row.emergency_fund_held, Some(usd(100_000)));
    assert_eq!(row.free_money, Some(usd(0)));
    assert_eq!(row.shortfall, Some(usd(0)));
}

#[test]
fn a_missing_balance_blocks_only_its_currency() {
    let konto = AccountInput {
        balance: None,
        ..cash("Konto", eur(0))
    };
    let id = konto.id;
    let answer = free(&ForecastInput {
        emergency_funds: fixed_fund(usd(300_000))
            .into_iter()
            .chain(fixed_fund(eur(0)))
            .collect(),
        accounts: vec![held("Checking", usd(1_000_000), JAN_15), konto],
        ..on(JAN_15, usd(0))
    });
    assert_eq!(
        answer.blockers,
        [Blocker::BalanceMissing {
            account: id,
            name: "Konto".to_owned(),
        }],
        "the mix of currencies blocks nothing here"
    );
    let codes: Vec<_> = answer.currencies.iter().map(|row| row.currency).collect();
    assert_eq!(codes, [currency("EUR"), currency("USD")], "sorted");
    let euros = &answer.currencies[0];
    assert_eq!(euros.free_money, None, "euros could hold anything");
    assert_eq!(euros.emergency_fund_held, None);
    assert_eq!(euros.shortfall, None);
    assert_eq!(dollars(&answer).free_money, Some(usd(700_000)));
    assert_eq!(answer.status, ForecastStatus::Provisional);
}

#[test]
fn no_counted_account_leaves_no_number() {
    let answer = free(&ForecastInput {
        accounts: vec![AccountInput {
            closed: true,
            ..held("Old", usd(10), JAN_15)
        }],
        items: vec![rent(CategoryId::new())],
        ..on(JAN_15, usd(0))
    });
    assert_eq!(answer.blockers, [Blocker::NoCountedAccounts]);
    assert_eq!(dollars(&answer).free_money, None);
    assert_eq!(
        dollars(&answer).still_need.total,
        usd(200_000),
        "still shown"
    );
    assert_eq!(answer.status, ForecastStatus::Unavailable);
}

#[test]
fn investments_never_count() {
    let without = on(JAN_15, usd(1_000_000));
    let mut with = without.clone();
    with.accounts.push(AccountInput {
        liquidity: LiquidityClass::Invested,
        ..held("Brokerage", usd(5_000_000), JAN_15)
    });
    assert_eq!(dollars(&free(&with)).cash_available, usd(1_000_000));
    assert_eq!(dollars(&free(&with)), dollars(&free(&without)));
}

#[test]
fn a_completed_goal_sets_aside_nothing() {
    let answer = free(&ForecastInput {
        items: vec![ItemInput {
            schedule: PlanSchedule::Goal {
                start_on: date(2026, 7, 15),
                due_on: date(2027, 7, 15),
                completed: true,
            },
            ..goal("Trip", usd(73_000), date(2026, 7, 15), date(2027, 7, 15), 0)
        }],
        ..on(JAN_15, usd(1_000_000))
    });
    assert_eq!(dollars(&answer).set_aside, usd(0));
}

/// What was spent from a goal comes off what it has set aside.
#[test]
fn spending_from_a_goal_lowers_what_it_set_aside() {
    let answer = free(&ForecastInput {
        items: vec![goal(
            "Trip",
            usd(73_000),
            date(2026, 7, 15),
            date(2027, 7, 15),
            10_000,
        )],
        ..on(JAN_15, usd(1_000_000))
    });
    assert_eq!(dollars(&answer).set_aside, usd(26_800));
}

/// A must pay item due only after the forecast's one-month window, but inside the 30 days,
/// still puts its currency in use, with the caution for its missing target.
#[test]
fn a_currency_only_due_soon_is_answered() {
    let today = date(2027, 1, 31);
    let answer = free(&ForecastInput {
        items: vec![one_off(
            "Rechnung",
            eur(5_000),
            date(2027, 3, 2),
            CategoryId::new(),
        )],
        ..on(today, usd(1_000_000))
    });
    let euros = answer
        .currencies
        .iter()
        .find(|row| row.currency == currency("EUR"))
        .unwrap();
    assert_eq!(euros.still_need.must_pay_coming, eur(5_000));
    assert_eq!(euros.shortfall, Some(eur(5_000)), "no euros to pay it");
    assert!(answer.cautions.contains(&Caution::EmergencyFundNotSet {
        currency: currency("EUR"),
    }));
}

#[test]
fn json_shape_is_stable() {
    let answer = free(&ForecastInput {
        items: vec![rent(CategoryId::new())],
        ..on(JAN_15, usd(1_000_000))
    });
    let json = serde_json::to_value(&answer).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "as_of": "2027-01-15",
            "must_pay_through": "2027-02-14",
            "status": "firm",
            "currencies": [{
                "currency": "USD",
                "cash_available": {"minor_units": 1_000_000, "currency": "USD"},
                "still_need": {
                    "must_pay_this_month": {"minor_units": 200_000, "currency": "USD"},
                    "must_pay_coming": {"minor_units": 0, "currency": "USD"},
                    "essentials": {"minor_units": 0, "currency": "USD"},
                    "flexible": {"minor_units": 0, "currency": "USD"},
                    "total": {"minor_units": 200_000, "currency": "USD"},
                },
                "set_aside": {"minor_units": 0, "currency": "USD"},
                "emergency_fund": {"kind": "fixed", "minor_units": 300_000},
                "emergency_fund_target": {"minor_units": 300_000, "currency": "USD"},
                "emergency_fund_held": {"minor_units": 300_000, "currency": "USD"},
                "free_money": {"minor_units": 500_000, "currency": "USD"},
                "shortfall": {"minor_units": 0, "currency": "USD"},
            }],
            "blockers": [],
            "cautions": [],
        })
    );
}
