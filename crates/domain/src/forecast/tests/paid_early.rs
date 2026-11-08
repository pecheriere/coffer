//! §10.8: a bill a payment has already paid, up to a week before its date, needs nothing from the
//! day that payment posts, so the money isn't counted twice: once gone from the balance and again
//! as still needed. Worked by hand from the dates and amounts here. Most cases are as of
//! 30 January 2027, with a month's window to 28 February, and a mortgage of 1,000 due on
//! 1 February.

use jiff::civil::{Date, date};

use super::{
    budget, every, fixed_fund, household, line, monthly, one_off, paid_on, run_as_given, spent,
    stored, usd, usd_totals,
};
use crate::forecast::{Forecast, ForecastInput, ItemInput, SpentTransaction};
use crate::ids::CategoryId;
use crate::plan::{CalendarMonth, IntervalUnit, SettlementKind};

const JAN_30: Date = date(2027, 1, 30);
const FEB_1: Date = date(2027, 2, 1);

/// As of `as_of`, with a window of `horizon_months` and an emergency fund of 0. The balance from
/// November isn't stale, so the only cautions are the bills'.
fn on(as_of: Date, horizon_months: u8) -> ForecastInput {
    ForecastInput {
        today: as_of,
        as_of,
        horizon_months,
        stale_after_days: 365,
        emergency_funds: fixed_fund(usd(0)),
        ..household()
    }
}

/// The mortgage, on its own, as of `as_of` with these transactions.
fn mortgage(as_of: Date, home: CategoryId, rows: Vec<SpentTransaction>) -> Forecast {
    run_as_given(&ForecastInput {
        items: vec![one_off("Mortgage", usd(1_000), FEB_1, home)],
        spent_transactions: rows,
        ..on(as_of, 1)
    })
}

/// The item's occurrences with the day they were paid: `None` is not paid.
fn paid_days(forecast: &Forecast, name: &str) -> Vec<(Date, Option<Date>)> {
    line(forecast, name)
        .payments
        .iter()
        .map(|one| (one.on, one.paid_by.map(|paid| paid.posted_on)))
        .collect()
}

/// Paid on 28 January, the mortgage of 1 February is already out of the balance: it needs nothing,
/// and the line says what paid it.
#[test]
fn a_bill_paid_early_needs_nothing() {
    let home = CategoryId::new();
    let forecast = mortgage(
        JAN_30,
        home,
        vec![paid_on(home, usd(1_000), date(2027, 1, 28))],
    );
    let mortgage = line(&forecast, "Mortgage");
    assert_eq!(mortgage.counted, usd(0));
    assert_eq!(mortgage.gross, usd(0));
    assert_eq!(mortgage.occurrences, 0);
    assert_eq!(
        paid_days(&forecast, "Mortgage"),
        [(FEB_1, Some(date(2027, 1, 28)))]
    );
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(0));
    assert_eq!(forecast.surplus, Some(usd(1_000_000)));
    assert_eq!(forecast.cautions, []);
}

/// §10.8: a bill paid on its own day, `as_of`, needs nothing either; it used to count in full.
#[test]
fn a_bill_paid_today_needs_nothing() {
    let home = CategoryId::new();
    let forecast = mortgage(FEB_1, home, vec![paid_on(home, usd(1_000), FEB_1)]);
    assert_eq!(line(&forecast, "Mortgage").counted, usd(0));
    assert_eq!(paid_days(&forecast, "Mortgage"), [(FEB_1, Some(FEB_1))]);
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(0));
}

/// As of 27 January the payment of the 28th hasn't posted, so the mortgage still needs all of it.
#[test]
fn before_the_payment_it_still_counts() {
    let home = CategoryId::new();
    let forecast = mortgage(date(2027, 1, 27), home, Vec::new());
    assert_eq!(line(&forecast, "Mortgage").counted, usd(1_000));
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(1_000));
}

/// Eight days before is too early to be the mortgage's, so it still counts, and the 1,000 stays in
/// the category's pool: the budget of 1,500 on the 20th needs 500 for January, and 1,500 more on
/// 20 February.
#[test]
fn eight_days_early_pays_nothing() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("Mortgage", usd(1_000), FEB_1, home),
            budget(monthly("Upkeep", usd(1_500), date(2027, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 1_000)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2027, 1, 24))],
        ..on(JAN_30, 1)
    });
    assert_eq!(line(&forecast, "Mortgage").counted, usd(1_000));
    assert_eq!(paid_days(&forecast, "Mortgage"), []);
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(1_000));
    assert_eq!(upkeep.counted, usd(2_000));
}

/// A monthly mortgage over two months from 30 January: December and January were paid on their
/// days and February's on 28 January, so only 1 March counts.
#[test]
fn only_the_paid_occurrence_needs_nothing() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![monthly("Mortgage", usd(1_000), date(2026, 12, 1), home)],
        spent_transactions: vec![
            paid_on(home, usd(1_000), date(2026, 12, 1)),
            paid_on(home, usd(1_000), date(2027, 1, 1)),
            paid_on(home, usd(1_000), date(2027, 1, 28)),
        ],
        ..on(JAN_30, 2)
    });
    let mortgage = line(&forecast, "Mortgage");
    assert_eq!(mortgage.counted, usd(1_000));
    assert_eq!(mortgage.occurrences, 1);
    assert_eq!(mortgage.first_on, Some(date(2027, 3, 1)));
    assert_eq!(
        paid_days(&forecast, "Mortgage"),
        [
            (date(2026, 12, 1), Some(date(2026, 12, 1))),
            (date(2027, 1, 1), Some(date(2027, 1, 1))),
            (FEB_1, Some(date(2027, 1, 28))),
        ]
    );
    assert_eq!(forecast.cautions, []);
}

/// Insurance every two months, on 1 December and 1 February. One payment on 28 January fits both,
/// and pays the older, unpaid one: so nothing is unpaid from last month, and February's still
/// counts in full.
#[test]
fn an_older_unpaid_bill_keeps_its_payment() {
    let home = CategoryId::new();
    let insurance = stored(
        "Insurance",
        usd(1_000),
        every(2, IntervalUnit::Months, date(2026, 12, 1)),
    );
    let forecast = run_as_given(&ForecastInput {
        items: vec![ItemInput {
            category: Some(home),
            ..insurance
        }],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2027, 1, 28))],
        ..on(date(2027, 1, 29), 1)
    });
    assert_eq!(
        paid_days(&forecast, "Insurance"),
        [(date(2026, 12, 1), Some(date(2027, 1, 28)))]
    );
    let insurance = line(&forecast, "Insurance");
    assert_eq!(insurance.unpaid, usd(0));
    assert_eq!(insurance.counted, usd(1_000), "1 February");
    assert_eq!(forecast.cautions, []);
}

/// The payment of the mortgage is out of the category's pool, so the budget of 500 on the 20th
/// isn't lowered by it: 500 for January and 500 for 20 February.
#[test]
fn an_early_payment_no_longer_lowers_the_budget() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("Mortgage", usd(1_000), FEB_1, home),
            budget(monthly("Upkeep", usd(500), date(2027, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 1_000)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2027, 1, 28))],
        ..on(JAN_30, 1)
    });
    assert_eq!(line(&forecast, "Mortgage").counted, usd(0));
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(0));
    assert_eq!(upkeep.counted, usd(1_000));
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(1_000));
}

/// Refunded the next day, the payment paid nothing, so the mortgage counts again.
#[test]
fn a_refund_undoes_an_early_payment() {
    let home = CategoryId::new();
    let forecast = mortgage(
        JAN_30,
        home,
        vec![
            paid_on(home, usd(1_000), date(2027, 1, 28)),
            paid_on(home, usd(-1_000), date(2027, 1, 29)),
        ],
    );
    assert_eq!(line(&forecast, "Mortgage").counted, usd(1_000));
    assert_eq!(paid_days(&forecast, "Mortgage"), []);
}

/// Water of 100 on the 10th, settled as paid for January, and the mortgage paid early, beside a
/// budget of 500 on the 20th, with 1,200 spent: 1,000 on the mortgage and 200 that fits no bill.
///
/// Unsettled, January's water is a gap of 100 and 10 February's counts; the pool is 200, so the
/// budget needs 300 + 500: 100 + 100 + 800 = 1,000. Settled, the water's 100 is charged to the
/// pool, leaving 100, but settling saved 100, so the pool keeps 200 - 100 = 100: 100 + 900. The
/// need doesn't rise.
#[test]
fn a_bill_settled_and_one_paid_early_in_one_pool() {
    let home = CategoryId::new();
    let mut water = monthly("Water", usd(100), date(2026, 12, 10), home);
    let mut input = ForecastInput {
        items: vec![
            water.clone(),
            one_off("Mortgage", usd(1_000), FEB_1, home),
            budget(monthly("Upkeep", usd(500), date(2027, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 1_200)],
        spent_transactions: vec![
            paid_on(home, usd(100), date(2026, 12, 10)),
            paid_on(home, usd(200), date(2027, 1, 12)),
            paid_on(home, usd(1_000), date(2027, 1, 28)),
        ],
        ..on(JAN_30, 1)
    };
    let need = |input: &ForecastInput| usd_totals(&run_as_given(input)).cash_needed;
    assert_eq!(need(&input), usd(1_000));

    let january: CalendarMonth = "2027-01".parse().unwrap();
    water.settled.insert(january, SettlementKind::Paid);
    input.items[0] = water;
    assert_eq!(need(&input), usd(1_000));
    assert_eq!(line(&run_as_given(&input), "Mortgage").counted, usd(0));
}

/// Matching reaches as far as a payment may be early: a bill a week after `as_of` is paid by a
/// payment posted on `as_of`, and needs nothing. Eight days after, the payment is too early, and
/// the bill counts in full. As of 15 November, with a month's window.
#[test]
fn matching_reaches_the_early_window() {
    let as_of = date(2026, 11, 15);
    for (due_on, paid) in [(date(2026, 11, 22), true), (date(2026, 11, 23), false)] {
        let home = CategoryId::new();
        let forecast = run_as_given(&ForecastInput {
            items: vec![one_off("Tyres", usd(1_000), due_on, home)],
            spent_transactions: vec![paid_on(home, usd(1_000), as_of)],
            ..on(as_of, 1)
        });
        let (days, counted) = if paid {
            (vec![(due_on, Some(as_of))], 0)
        } else {
            (Vec::new(), 1_000)
        };
        assert_eq!(paid_days(&forecast, "Tyres"), days, "due on {due_on}");
        assert_eq!(line(&forecast, "Tyres").counted, usd(counted));
    }
}

/// The mortgage of 1 February, next month, settled as paid for February, and paid on 28 January:
/// that payment is the mortgage's, settled or not, so it stays off the budget of 500 on the 20th,
/// which needs all 500 for January and 500 for 20 February, as it would in the same month.
#[test]
fn a_bill_next_month_settled_as_paid_keeps_its_early_payment_off_the_budget() {
    let home = CategoryId::new();
    let mut mortgage = one_off("Mortgage", usd(1_000), FEB_1, home);
    let february: CalendarMonth = "2027-02".parse().unwrap();
    mortgage.settled.insert(february, SettlementKind::Paid);
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            mortgage,
            budget(monthly("Upkeep", usd(500), date(2027, 1, 20), home)),
        ],
        category_spent: vec![spent(home, 1_000)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2027, 1, 28))],
        ..on(JAN_30, 1)
    });
    let upkeep = line(&forecast, "Upkeep");
    assert_eq!(upkeep.reduced_by, usd(0));
    assert_eq!(upkeep.counted, usd(1_000));
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(1_000));
}
