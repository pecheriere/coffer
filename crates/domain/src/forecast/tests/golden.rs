//! The worked example the whole product rests on.
//!
//! `as_of` 2026-11-15, three months, so the window ends before 2027-02-15.
//! Cash: 2,000,000 + 5,000,000 - 150,000 = 6,850,000; Brokerage and Side stay out.
//! Rent, a bill on the 1st: Dec, Jan, Feb = 600,000. Nov 1's 200,000 is covered by
//! the 200,000 spent on Housing, so nothing is unpaid.
//! Groceries, a budget on the 28th: Nov, Dec, Jan = 180,000, less the Food pool of
//! 40,000.
//! Water, a bill every 2 months from January: Jan 10 in the window = 12,000, and
//! Nov 10 has passed with nothing spent on Utilities: 12,000 unpaid, with a caution.
//! Repair: 150,000.
//! Outflows: 600,000 + 140,000 + 24,000 + 150,000 = 914,000.
//! Vacation is due in the window: 600,000 - 100,000 = 500,000. Roof is 365 of 730
//! days along at the end: 600,000. Goals: 1,100,000.
//! Needed: 914,000 + 1,100,000 + 1,000,000 = 3,014,000. Surplus: 3,836,000.
//! With 12,000 spent on Utilities, as `golden_with_the_water_bill_seen` has it: no
//! gap, outflows 902,000, needed 3,002,000, surplus 3,848,000, firm.

use jiff::civil::{Date, date};

use super::{
    AS_OF, assert_provisional, budget, cash, currency, every, fixed_floor, goal, household, inflow,
    line, monthly, one_off, run, totals_of, usd, usd_totals,
};
use crate::accounts::LiquidityClass;
use crate::forecast::{
    AccountInput, BalanceInput, CategorySpent, Caution, ForecastInput, ForecastStatus, ItemInput,
    MonthItem, NotCountedReason,
};
use crate::ids::CategoryId;
use crate::plan::{CalendarMonth, IntervalUnit};

fn golden_household() -> ForecastInput {
    let housing = CategoryId::new();
    let food = CategoryId::new();
    let utilities = CategoryId::new();
    let car = CategoryId::new();
    let travel = CategoryId::new();
    let home = CategoryId::new();
    let pay = CategoryId::new();
    ForecastInput {
        floor: Some(fixed_floor(usd(1_000_000))),
        accounts: vec![
            cash("Checking", usd(2_000_000)),
            cash("Savings", usd(5_000_000)),
            AccountInput {
                liquidity: LiquidityClass::ShortTermLiability,
                ..cash("Card", usd(-150_000))
            },
            AccountInput {
                liquidity: LiquidityClass::Invested,
                ..cash("Brokerage", usd(9_000_000))
            },
            AccountInput {
                include_in_forecast: false,
                ..cash("Side", usd(100_000))
            },
        ],
        items: vec![
            monthly("Rent", usd(200_000), date(2026, 1, 1), housing),
            budget(monthly("Groceries", usd(60_000), date(2026, 1, 28), food)),
            ItemInput {
                category: Some(utilities),
                ..super::stored(
                    "Water",
                    usd(12_000),
                    every(2, IntervalUnit::Months, date(2026, 1, 10)),
                )
            },
            one_off("Repair", usd(150_000), date(2026, 12, 20), car),
            one_off("Late", usd(99_900), date(2027, 2, 15), car),
            ItemInput {
                category: Some(travel),
                ..goal(
                    "Vacation",
                    usd(600_000),
                    date(2026, 6, 1),
                    date(2027, 1, 31),
                    100_000,
                )
            },
            ItemInput {
                category: Some(home),
                ..goal(
                    "Roof",
                    usd(1_200_000),
                    date(2026, 2, 15),
                    date(2028, 2, 15),
                    0,
                )
            },
            inflow(monthly("Salary", usd(800_000), date(2026, 1, 25), pay)),
        ],
        category_spent: vec![
            CategorySpent {
                category: housing,
                currency: currency("USD"),
                spent_minor: 200_000,
            },
            CategorySpent {
                category: food,
                currency: currency("USD"),
                spent_minor: 40_000,
            },
        ],
        ..household()
    }
}

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

#[test]
fn golden_three_month_forecast() {
    let forecast = run(&golden_household());

    assert_eq!(forecast.as_of, AS_OF, "the window starts on as_of");
    assert_eq!(forecast.end_exclusive, date(2027, 2, 15), "three months on");
    assert_provisional(&forecast);
    assert_eq!(forecast.surplus, Some(usd(3_836_000)), "the answer");
    assert_eq!(forecast.blockers, [], "nothing blocks");
    assert_eq!(
        forecast.cautions,
        [Caution::BillNotSeen {
            category: line(&forecast, "Water").category.unwrap(),
            amount: usd(12_000),
        }],
        "November's water bill has not been seen"
    );

    assert_eq!(forecast.totals.len(), 1, "one currency");
    let totals = usd_totals(&forecast);
    assert_eq!(totals.cash_available, usd(6_850_000), "cash available");
    assert_eq!(totals.planned_outflows, usd(914_000), "planned outflows");
    assert_eq!(totals.goals, usd(1_100_000), "goals");
    assert_eq!(totals.floor, usd(1_000_000), "floor");
    assert_eq!(totals.planned_inflows, usd(0), "income is off");
    assert_eq!(totals.excluded_inflows, usd(2_400_000), "income left out");
    assert_eq!(totals.cash_needed, usd(3_014_000), "cash needed");
    assert_eq!(
        totals.surplus,
        Some(usd(3_836_000)),
        "the currency's surplus"
    );

    let reasons: Vec<_> = forecast
        .accounts
        .iter()
        .map(|account| (account.name.as_str(), account.counted, account.not_counted))
        .collect();
    assert_eq!(
        reasons,
        [
            ("Checking", true, None),
            ("Savings", true, None),
            ("Card", true, None),
            ("Brokerage", false, Some(NotCountedReason::NotCash)),
            ("Side", false, Some(NotCountedReason::ExcludedByFlag)),
        ],
        "each account says whether it counted"
    );
    let card = forecast.accounts.get(2).unwrap();
    assert_eq!(card.balance, Some(usd(-150_000)), "owing stays negative");
    assert_eq!(card.balance_on, Some(date(2026, 11, 14)), "its date");

    let rent = line(&forecast, "Rent");
    assert_eq!(rent.occurrences, 3, "December, January, February");
    assert_eq!(
        rent.first_on,
        Some(date(2026, 12, 1)),
        "first in the window"
    );
    assert_eq!(rent.last_on, Some(date(2027, 2, 1)), "last in the window");
    assert_eq!(rent.gross, usd(600_000), "rent gross");
    assert_eq!(rent.reduced_by, usd(0), "November's rent ate the pool");
    assert_eq!(rent.counted, usd(600_000), "rent counted");

    let groceries = line(&forecast, "Groceries");
    assert_eq!(groceries.gross, usd(180_000), "groceries gross");
    assert_eq!(groceries.reduced_by, usd(40_000), "spent so far");
    assert_eq!(groceries.counted, usd(140_000), "groceries counted");

    let water = line(&forecast, "Water");
    assert_eq!(water.gross, usd(12_000), "January only in the window");
    assert_eq!(water.unpaid, usd(12_000), "November's not seen");
    assert_eq!(water.counted, usd(24_000), "both count");
    assert_eq!(line(&forecast, "Repair").counted, usd(150_000), "one-off");
    assert!(
        !forecast.items.iter().any(|line| line.name == "Late"),
        "the end of the window is excluded"
    );

    let vacation = line(&forecast, "Vacation");
    assert_eq!(vacation.counted, usd(500_000), "the remaining target");
    let vacation_goal = vacation.goal.unwrap();
    assert_eq!(vacation_goal.target, usd(600_000), "target");
    assert_eq!(vacation_goal.spent, usd(100_000), "spent");
    assert_eq!(vacation_goal.due_on, date(2027, 1, 31), "due date");
    assert!(vacation_goal.due_in_horizon, "due in the window");

    let roof = line(&forecast, "Roof");
    assert_eq!(roof.counted, usd(600_000), "halfway through at the end");
    assert_eq!(roof.occurrences, 0, "a goal has no occurrences");
    assert_eq!(roof.first_on, None, "a goal has no occurrences");
    assert!(!roof.goal.unwrap().due_in_horizon, "due after the window");

    let salary = line(&forecast, "Salary");
    assert_eq!(salary.gross, usd(2_400_000), "three pay days");
    assert_eq!(salary.counted, usd(0), "income is off");
}

/// Vacation is 244 days, spent 100,000. Accrued less spent: Dec 1 (183 days)
/// 450,000 - 100,000 = 350,000; Jan 1 (214 days) 526,230 - 100,000 = 426,230, so
/// December adds 76,230; past due it is 500,000, so January adds 73,770.
/// Roof is 730 days: Dec 1 (289) 475,068; Jan 1 (320) 526,027, so December adds
/// 50,959; Feb 1 (351) 576,986, so January adds 50,959; Feb 15 (365) 600,000, so
/// February adds 23,014.
const GOLDEN_MONTHS: [(&str, bool, i64, i64, i64, i64); 4] = [
    ("2026-11", true, 32_000, 800_000, 825_068, 6_818_000),
    ("2026-12", false, 410_000, 800_000, 127_189, 6_408_000),
    ("2027-01", false, 272_000, 800_000, 124_729, 6_136_000),
    ("2027-02", true, 200_000, 0, 23_014, 5_936_000),
];

#[test]
fn golden_months_break_the_totals_down() {
    let forecast = run(&golden_household());
    assert_eq!(forecast.months.len(), GOLDEN_MONTHS.len(), "four buckets");
    for (bucket, (text, partial, outflows, inflows, accrual, projected)) in
        forecast.months.iter().zip(GOLDEN_MONTHS)
    {
        assert_eq!(bucket.month, month(text), "buckets run in order");
        assert_eq!(bucket.partial, partial, "first and last are partial");
        let [line] = bucket.lines.as_slice() else {
            panic!("one currency means one line");
        };
        assert_eq!(line.currency, currency("USD"), "its currency");
        assert_eq!(line.outflows, usd(outflows), "month outflows");
        assert_eq!(line.inflows, usd(inflows), "month inflows");
        assert_eq!(line.goal_accrual, usd(accrual), "month goal accrual");
        assert_eq!(line.projected_cash, usd(projected), "projected cash");
    }
}

#[test]
fn golden_month_items_trace_to_their_sources() {
    let forecast = run(&golden_household());
    let source_of = |name: &str| line(&forecast, name).source;
    let november = forecast.months.first().unwrap().lines.first().unwrap();
    let item = |name: &str, minor| MonthItem {
        source: source_of(name),
        amount: usd(minor),
    };
    assert_eq!(
        november.items,
        [
            item("Groceries", 20_000),
            item("Water", 12_000),
            item("Vacation", 350_000),
            item("Roof", 475_068),
            item("Salary", 800_000),
        ],
        "November's lines, in input order, without zeros"
    );
    let february = forecast.months.last().unwrap().lines.first().unwrap();
    assert_eq!(
        february.items,
        [item("Rent", 200_000), item("Roof", 23_014)],
        "a goal finished earlier adds nothing"
    );
}

#[test]
fn golden_with_income() {
    let forecast = run(&ForecastInput {
        include_income: true,
        ..golden_household()
    });
    let totals = usd_totals(&forecast);
    assert_eq!(totals.planned_inflows, usd(2_400_000), "income counted");
    assert_eq!(totals.excluded_inflows, usd(0), "nothing left out");
    assert_eq!(totals.cash_needed, usd(614_000), "income lowers the need");
    assert_eq!(forecast.surplus, Some(usd(6_236_000)), "the answer");
    assert_provisional(&forecast);
    assert_eq!(
        line(&forecast, "Salary").counted,
        usd(2_400_000),
        "income counted on its line"
    );
    let projected: Vec<_> = forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().projected_cash)
        .collect();
    assert_eq!(
        projected,
        [
            usd(7_618_000),
            usd(8_008_000),
            usd(8_536_000),
            usd(8_336_000)
        ],
        "6,850,000 - 32,000 + 800,000, and so on"
    );
}

/// The same household with 12,000 spent on Utilities, so every bill has been seen:
/// outflows 600,000 + 140,000 + 12,000 + 150,000 = 902,000, needed 3,002,000.
#[test]
fn golden_with_the_water_bill_seen() {
    let mut input = golden_household();
    let utilities = input
        .items
        .iter()
        .find(|item| item.name == "Water")
        .and_then(|item| item.category)
        .unwrap();
    input.category_spent.push(CategorySpent {
        category: utilities,
        currency: currency("USD"),
        spent_minor: 12_000,
    });
    let forecast = run(&input);
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing to warn of");
    assert_eq!(forecast.cautions, [], "no caution");
    assert_eq!(forecast.surplus, Some(usd(3_848_000)), "the answer");
    let totals = usd_totals(&forecast);
    assert_eq!(totals.planned_outflows, usd(902_000), "planned outflows");
    assert_eq!(totals.cash_needed, usd(3_002_000), "cash needed");
    let projected: Vec<_> = forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().projected_cash)
        .collect();
    assert_eq!(
        projected,
        [
            usd(6_830_000),
            usd(6_420_000),
            usd(6_148_000),
            usd(5_948_000)
        ],
        "6,850,000 less each month's outflows"
    );
}

#[test]
fn month_inflows_show_income_that_is_not_counted() {
    let forecast = run(&golden_household());
    let november = forecast.months.first().unwrap().lines.first().unwrap();
    assert_eq!(november.inflows, usd(800_000), "shown though not counted");
    assert_eq!(
        november.projected_cash,
        usd(6_818_000),
        "and not in projected cash"
    );
    assert_eq!(
        totals_of(&forecast, "USD").planned_inflows,
        usd(0),
        "nor in the totals"
    );
}

#[test]
fn balances_are_carried_as_given() {
    const ON: Date = date(2026, 11, 2);
    let mut input = household();
    let mut account = cash("Old", usd(5));
    account.balance = Some(BalanceInput {
        on: ON,
        minor_units: 5,
    });
    input.stale_after_days = 365;
    input.accounts = vec![account];
    let forecast = run(&input);
    let line = forecast.accounts.first().unwrap();
    assert_eq!(line.balance, Some(usd(5)), "the balance");
    assert_eq!(line.balance_on, Some(ON), "its date");
}
