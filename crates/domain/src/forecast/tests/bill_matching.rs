//! R9b: which transaction paid which occurrence of a dated bill, worked by hand from the dates and
//! amounts here. `as_of` is 15 November 2026 and a month's window runs to 15 December unless a test
//! says otherwise, so a monthly bill on the 5th has 5 October and 5 November behind it and
//! 5 December ahead.

use jiff::civil::{Date, date};

use super::{
    budget, fixed_floor, household, hypothetical, line, monthly, one_off, paid_on, run_as_given,
    usd,
};
use crate::forecast::{Caution, Forecast, ForecastInput, ForecastStatus, LooseEnds};
use crate::ids::CategoryId;
use crate::money::Money;
use crate::plan::{CalendarMonth, SettlementKind};

fn one_month() -> ForecastInput {
    ForecastInput {
        horizon_months: 1,
        floor: Some(fixed_floor(usd(0))),
        ..household()
    }
}

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

/// The item's occurrences with the day they were paid: `None` is not paid.
fn paid_days(forecast: &Forecast, name: &str) -> Vec<(Date, Option<Date>)> {
    line(forecast, name)
        .payments
        .iter()
        .map(|one| (one.on, one.paid_by.map(|paid| paid.posted_on)))
        .collect()
}

/// The 28 October bill, paid on 2 November, is October's: November's, on the 28th, is still to
/// come and counts in full. Cash 1,000,000 less that one bill of 1,000 is 999,000.
#[test]
fn a_bill_paid_on_the_2nd_pays_last_month() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![monthly("Rent", usd(1_000), date(2026, 10, 28), home)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 2))],
        ..one_month()
    });
    assert_eq!(
        paid_days(&forecast, "Rent"),
        [(date(2026, 10, 28), Some(date(2026, 11, 2)))]
    );
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.unpaid, usd(0));
    assert_eq!(rent.counted, usd(1_000), "28 November");
    assert_eq!(forecast.cautions, []);
    assert_eq!(forecast.surplus, Some(usd(999_000)));
    assert_eq!(forecast.status, ForecastStatus::Firm);
}

/// Unpaid and past, last month's bill counts and is named, without touching this month's total.
/// 1,000,000 - 1,000 (28 October) - 1,000 (28 November) = 998,000.
#[test]
fn a_bill_unpaid_from_last_month_still_counts_and_is_named() {
    let home = CategoryId::new();
    let rent = monthly("Rent", usd(1_000), date(2026, 10, 28), home);
    let source = rent.source;
    let forecast = run_as_given(&ForecastInput {
        items: vec![rent],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(1_000));
    assert_eq!(
        forecast.cautions,
        [Caution::BillUnpaidLastMonth {
            item: source,
            on: date(2026, 10, 28),
            amount: usd(1_000),
        }]
    );
    assert_eq!(forecast.surplus, Some(usd(998_000)));
    assert_eq!(forecast.status, ForecastStatus::Provisional);
}

/// Two months and one payment: it pays 5 October, the oldest, though it posted on 6 November, and
/// 5 November is the one still owed. 1,000,000 - 1,000 (5 November) - 1,000 (5 December).
#[test]
fn one_payment_for_two_months_pays_the_older() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 5), home)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 6))],
        ..one_month()
    });
    assert_eq!(
        paid_days(&forecast, "Rent"),
        [
            (date(2026, 10, 5), Some(date(2026, 11, 6))),
            (date(2026, 11, 5), None),
        ]
    );
    assert_eq!(line(&forecast, "Rent").unpaid, usd(1_000));
    assert_eq!(
        forecast.cautions,
        [super::not_seen(&forecast, home, usd(1_000))],
        "this month's, by category"
    );
    assert_eq!(forecast.surplus, Some(usd(998_000)));
}

/// A monthly bill of 1,000 and a budget of 500 in one category, with 400 spent. It can't be the
/// bill, so it is the budget's: 1,000 still owed, 100 left of the budget and 1,000 ahead. Taken
/// as part of the bill, as it once was, 600 would have been owed and 500 left: the same money, with
/// the bill hidden.
#[test]
fn spending_on_a_budget_cannot_hide_a_bill() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            monthly("Rent", usd(1_000), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(500), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 400)],
        spent_transactions: vec![paid_on(home, usd(400), date(2026, 11, 6))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(1_000));
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(400));
    assert_eq!(line(&forecast, "Upkeep").counted, usd(100));
    assert_eq!(forecast.surplus, Some(usd(997_900)), "1,000,000 - 2,100");
}

/// The exact amount tells a payment from the budget, so it pays the bill and leaves the budget
/// whole, though the budget could have spent it.
#[test]
fn the_exact_amount_is_the_bill_even_beside_a_budget() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            monthly("Sub", usd(1_000), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(5_000), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 1_000)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Sub").unpaid, usd(0));
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(0));
    assert_eq!(forecast.cautions, []);
}

/// 95 is close to the bill of 100, but the budget of 400 could have spent it. So the bill stays
/// owed, the money stays the budget's, and the answer says so. Needed: 100 + 305 + 100 = 505,
/// where the truth is 500 if it was the bill and 505 if it wasn't: never less than either.
#[test]
fn a_transaction_that_could_be_either_is_said_and_pays_nothing() {
    let home = CategoryId::new();
    let bill = monthly("Sub", usd(100), date(2026, 11, 5), home);
    let source = bill.source;
    let row = paid_on(home, usd(95), date(2026, 11, 5));
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            bill,
            budget(monthly("Upkeep", usd(400), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 95)],
        spent_transactions: vec![row],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Sub").unpaid, usd(100));
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(95));
    assert_eq!(
        forecast.cautions,
        [
            super::not_seen(&forecast, home, usd(100)),
            Caution::BillPaymentUnclear {
                item: source,
                on: date(2026, 11, 5),
                amount: usd(100),
                transaction: row.transaction,
                transaction_amount: usd(95),
            },
        ]
    );
    assert_eq!(forecast.surplus, Some(usd(999_495)));
}

/// More than the budget plans for its whole month isn't the budget's: 1,250 is the rent of 1,300,
/// within 10%, beside a budget of 300.
#[test]
fn more_than_a_budget_plans_is_the_bill() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            monthly("Rent", usd(1_300), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(300), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 1_250)],
        spent_transactions: vec![paid_on(home, usd(1_250), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0));
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(0));
    assert_eq!(forecast.cautions, []);
}

/// 10% either way pays it, and a minor unit more doesn't.
#[test]
fn the_tolerance_is_ten_percent_either_way() {
    for (spent, unpaid) in [(900, 0), (1_100, 0), (899, 1_000), (1_101, 1_000)] {
        let home = CategoryId::new();
        let forecast = run_as_given(&ForecastInput {
            items: vec![one_off("Tyres", usd(1_000), date(2026, 11, 5), home)],
            spent_transactions: vec![paid_on(home, usd(spent), date(2026, 11, 6))],
            ..one_month()
        });
        assert_eq!(line(&forecast, "Tyres").unpaid, usd(unpaid));
    }
}

/// A transaction pays one occurrence: two bills of one amount on one day take two payments, the
/// first in input order first.
#[test]
fn a_transaction_pays_one_occurrence_at_most() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("First", usd(1_000), date(2026, 11, 5), home),
            one_off("Second", usd(1_000), date(2026, 11, 5), home),
        ],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "First").unpaid, usd(0));
    assert_eq!(line(&forecast, "Second").unpaid, usd(1_000));
}

/// A week early counts, for a debit moved around a weekend, and eight days doesn't: the second
/// payment of October isn't November's first.
#[test]
fn a_payment_can_be_a_week_early_and_no_more() {
    for (posted_on, unpaid) in [(date(2026, 11, 3), 0), (date(2026, 11, 2), 1_000)] {
        let home = CategoryId::new();
        let forecast = run_as_given(&ForecastInput {
            items: vec![one_off("Tyres", usd(1_000), date(2026, 11, 10), home)],
            spent_transactions: vec![paid_on(home, usd(1_000), posted_on)],
            ..one_month()
        });
        assert_eq!(line(&forecast, "Tyres").unpaid, usd(unpaid));
    }
}

/// A refund within the tolerance of a payment undoes it, so the bill is owed again.
#[test]
fn a_refund_of_about_the_payment_undoes_it() {
    for refund in [1_000, 950] {
        let home = CategoryId::new();
        let forecast = run_as_given(&ForecastInput {
            items: vec![one_off("Tyres", usd(1_000), date(2026, 11, 5), home)],
            spent_transactions: vec![
                paid_on(home, usd(1_000), date(2026, 11, 5)),
                paid_on(home, usd(-refund), date(2026, 11, 8)),
            ],
            ..one_month()
        });
        assert_eq!(line(&forecast, "Tyres").unpaid, usd(1_000));
    }
}

/// A refund of 250 after a rent of 2,000 is something else coming back: the rent stays paid, and
/// the refund only lowers what the category spent, so the budget has 300 - 250 of it.
#[test]
fn a_refund_outside_the_tolerance_unpays_nothing() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("Rent", usd(2_000), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(500), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 2_050)],
        spent_transactions: vec![
            paid_on(home, usd(2_000), date(2026, 11, 5)),
            paid_on(home, usd(300), date(2026, 11, 6)),
            paid_on(home, usd(-250), date(2026, 11, 8)),
        ],
        ..one_month()
    });
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.unpaid, usd(0));
    assert_eq!(rent.payments[0].paid_by.unwrap().amount, usd(2_000));
    assert_eq!(line(&forecast, "Upkeep").reduced_by, usd(50), "300 - 250");
}

/// 1,800 pays a bill of 2,000, and the line shows the 200 it didn't cover.
#[test]
fn a_payment_off_by_less_than_the_tolerance_shows_the_difference() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![one_off("Rent", usd(2_000), date(2026, 11, 5), home)],
        spent_transactions: vec![paid_on(home, usd(1_800), date(2026, 11, 5))],
        ..one_month()
    });
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.unpaid, usd(0));
    assert_eq!(rent.payments[0].paid_by.unwrap().difference, usd(-200));
    assert_eq!(forecast.cautions, []);
}

/// One category with a settled bill and a budget of `budget_minor` on the 20th, what was spent, and
/// the budget's line.
fn settled_beside_a_budget(
    bill: Money,
    on: Date,
    budget_minor: i64,
    spent: &[(i64, Date)],
) -> Forecast {
    let home = CategoryId::new();
    let mut item = one_off("Bill", bill, on, home);
    item.settled
        .insert(CalendarMonth::of(on).unwrap(), SettlementKind::Paid);
    run_as_given(&ForecastInput {
        items: vec![
            item,
            budget(monthly(
                "Upkeep",
                usd(budget_minor),
                date(2026, 11, 20),
                home,
            )),
        ],
        category_spent: vec![super::spent(home, spent.iter().map(|one| one.0).sum())],
        spent_transactions: spent
            .iter()
            .map(|&(minor, posted_on)| paid_on(home, usd(minor), posted_on))
            .collect(),
        ..one_month()
    })
}

/// Settled as paid, the money it was paid with is no budget's, whether or not a transaction fits:
/// charged 230 on a bill of 200, only the 30 beyond the bill can be the budget's, so it needs 120.
/// It can't take the whole 230, since then settling could raise the need.
#[test]
fn a_settled_bill_keeps_a_charge_that_does_not_fit_off_the_budget() {
    let forecast = settled_beside_a_budget(
        usd(200),
        date(2026, 11, 5),
        150,
        &[(230, date(2026, 11, 5))],
    );
    assert_eq!(line(&forecast, "Upkeep").counted, usd(120));
}

/// Paid in two parts: neither fits, and together they are the rent, so the budget needs all 500.
#[test]
fn a_settled_bill_paid_in_two_parts_leaves_the_budget_whole() {
    let forecast = settled_beside_a_budget(
        usd(2_000),
        date(2026, 11, 5),
        500,
        &[(1_000, date(2026, 11, 5)), (1_000, date(2026, 11, 6))],
    );
    assert_eq!(line(&forecast, "Upkeep").counted, usd(500));
}

/// Due on the 20th, after `as_of`, and settled: paid on the 14th, or a fortnight before, that money
/// is the bill's, so the budget needs all 500.
#[test]
fn a_bill_settled_later_this_month_keeps_its_payment_off_the_budget() {
    for posted_on in [date(2026, 11, 14), date(2026, 11, 2)] {
        let forecast =
            settled_beside_a_budget(usd(1_000), date(2026, 11, 20), 500, &[(1_000, posted_on)]);
        assert_eq!(line(&forecast, "Upkeep").counted, usd(500));
    }
}

/// 480 could be the bill of 450 or the budget's, so it is said. If it was the bill the budget needs
/// all 500; if not, the bill is 450 and the budget 20. Leaving only 450 with the budget holds
/// 450 + 50 = 500, not the 470 that leaving all of it would.
#[test]
fn an_unclear_transaction_leaves_no_more_than_the_bill_with_the_budget() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            one_off("Sub", usd(450), date(2026, 11, 5), home),
            budget(monthly("Upkeep", usd(500), date(2026, 11, 20), home)),
        ],
        category_spent: vec![super::spent(home, 480)],
        spent_transactions: vec![paid_on(home, usd(480), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Sub").unpaid, usd(450));
    assert_eq!(line(&forecast, "Upkeep").counted, usd(50));
    assert_eq!(forecast.surplus, Some(usd(999_500)));
}

/// A row before the look-back can pay the first bill inside it: debited 30 September for 1 October,
/// and 30 October for 1 November, so nothing is owed.
#[test]
fn a_payment_just_before_the_look_back_pays_its_first_bill() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 1), home)],
        spent_transactions: vec![
            paid_on(home, usd(1_000), date(2026, 9, 30)),
            paid_on(home, usd(1_000), date(2026, 10, 30)),
        ],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0));
    assert_eq!(forecast.cautions, []);
}

/// A weekly bill: the row of 24 September is that week's, exactly 7 days before 1 October, so it
/// doesn't pay 1 October. Moved along by one, it would hide the week that was missed. Two days before
/// 1 October is nearer it than 24 September, so it does.
#[test]
fn a_payment_before_the_look_back_stays_with_a_nearer_occurrence() {
    let weekly = |home| {
        let mut item = monthly("Sitter", usd(1_000), date(2026, 9, 24), home);
        item.schedule = crate::plan::PlanSchedule::Recurring {
            every: 1,
            unit: crate::plan::IntervalUnit::Weeks,
            anchor_on: date(2026, 9, 24),
            end_on: Some(date(2026, 10, 1)),
        };
        item
    };
    for (posted_on, unpaid) in [(date(2026, 9, 24), 1_000), (date(2026, 9, 29), 0)] {
        let home = CategoryId::new();
        let forecast = run_as_given(&ForecastInput {
            items: vec![weekly(home)],
            spent_transactions: vec![paid_on(home, usd(1_000), posted_on)],
            ..one_month()
        });
        assert_eq!(line(&forecast, "Sitter").unpaid, usd(unpaid));
    }
}

/// History starts on 1 November, so October's rent has no evidence either way: not unpaid, in no
/// total, and one caution says how much. It takes no payment, so the 5 November charge is
/// November's, not October's. Only 5 December counts.
#[test]
fn a_bill_before_the_first_transaction_is_not_treated_as_unpaid() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 5), home)],
        earliest_transaction: Some(date(2026, 11, 1)),
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 5))],
        ..one_month()
    });
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.unpaid, usd(0));
    assert!(rent.payments[0].before_history);
    assert_eq!(
        rent.payments[1].paid_by.unwrap().posted_on,
        date(2026, 11, 5)
    );
    assert_eq!(forecast.surplus, Some(usd(999_000)));
    assert_eq!(
        forecast.cautions,
        [Caution::BillHistoryMissing {
            starts_on: Some(date(2026, 11, 1)),
            currency: super::currency("USD"),
            occurrences: 1,
            amount: usd(1_000),
        }]
    );
}

/// With no transactions at all, nothing can say a bill wasn't paid either, but today's still counts.
#[test]
fn with_no_transactions_nothing_is_treated_as_unpaid() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![one_off("Tyres", usd(1_000), date(2026, 11, 5), home)],
        earliest_transaction: None,
        ..one_month()
    });
    assert_eq!(forecast.surplus, Some(usd(1_000_000)));
    assert_eq!(forecast.status, ForecastStatus::Provisional);
}

/// A bill unpaid from last month, with an uncategorized transaction last month: it may be the payment,
/// so the answer says to file it. With last month's bill paid, it says nothing.
#[test]
fn last_months_loose_ends_are_said_only_with_a_bill_unpaid() {
    let loose = LooseEnds {
        currency: super::currency("USD"),
        uncategorized_count: 1,
        uncategorized_minor: 1_000,
        possible_duplicates: 0,
    };
    let said = Caution::LastMonthLooseEnds {
        currency: super::currency("USD"),
        uncategorized_count: 1,
        uncategorized_total: usd(1_000),
        possible_duplicates: 0,
    };
    let home = CategoryId::new();
    let unpaid = run_as_given(&ForecastInput {
        items: vec![one_off("Tyres", usd(1_000), date(2026, 10, 20), home)],
        last_month_loose_ends: vec![loose],
        ..one_month()
    });
    assert!(unpaid.cautions.contains(&said));
    let paid = run_as_given(&ForecastInput {
        items: vec![one_off("Tyres", usd(1_000), date(2026, 10, 20), home)],
        last_month_loose_ends: vec![loose],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 10, 20))],
        ..one_month()
    });
    assert!(!paid.cautions.contains(&said));
}

/// A settled October is out of it, so the one payment goes on to November: how a member corrects
/// a match they know is wrong. Only 5 December is left to count.
#[test]
fn settling_a_month_sends_its_payment_to_the_next() {
    let home = CategoryId::new();
    let mut rent = monthly("Rent", usd(1_000), date(2026, 1, 5), home);
    rent.settled.insert(month("2026-10"), SettlementKind::Paid);
    let forecast = run_as_given(&ForecastInput {
        items: vec![rent],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 6))],
        ..one_month()
    });
    assert_eq!(
        paid_days(&forecast, "Rent"),
        [(date(2026, 11, 5), Some(date(2026, 11, 6)))],
        "October isn't listed here, but in `settled`"
    );
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0));
    assert_eq!(forecast.settled.len(), 1);
    assert_eq!(forecast.settled[0].month, month("2026-10"));
    assert_eq!(forecast.surplus, Some(usd(999_000)));
    assert_eq!(forecast.cautions, []);
}

/// An unclear transaction a person says was the bill leaves the budget, or the same 95 would be
/// taken off twice: the bill's 100 goes, and the budget is whole. 400 + 100 on 5 December. Skipped,
/// it was no payment, so the budget keeps it: 305 + 100.
#[test]
fn a_paid_settlement_takes_the_unclear_transaction_off_the_budget() {
    for (kind, surplus) in [
        (SettlementKind::Paid, 999_500),
        (SettlementKind::Skipped, 999_595),
    ] {
        let home = CategoryId::new();
        let mut bill = monthly("Sub", usd(100), date(2026, 11, 5), home);
        bill.settled.insert(month("2026-11"), kind);
        let forecast = run_as_given(&ForecastInput {
            items: vec![
                bill,
                budget(monthly("Upkeep", usd(400), date(2026, 11, 20), home)),
            ],
            category_spent: vec![super::spent(home, 95)],
            spent_transactions: vec![paid_on(home, usd(95), date(2026, 11, 5))],
            ..one_month()
        });
        assert_eq!(forecast.surplus, Some(usd(surplus)));
        assert_eq!(forecast.cautions, []);
    }
}

/// On the first of a month all of last month is behind and nothing of this one is: 1 November's own
/// bill counts in full and owes nothing, and 1 October, last month's first day, is looked at.
#[test]
fn on_the_first_of_a_month_all_of_last_month_is_looked_at() {
    let as_of = date(2026, 11, 1);
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        today: as_of,
        as_of,
        items: vec![monthly("Rent", usd(1_000), date(2026, 1, 1), home)],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 10, 1))],
        ..one_month()
    });
    assert_eq!(
        paid_days(&forecast, "Rent"),
        [(date(2026, 10, 1), Some(date(2026, 10, 1))), (as_of, None),],
        "September's is too long ago"
    );
    let rent = line(&forecast, "Rent");
    assert_eq!(rent.unpaid, usd(0), "today's is still to come");
    assert_eq!(rent.counted, usd(1_000), "and counts, once");
    assert_eq!(forecast.cautions, []);
}

/// On the last of a month, a month-end bill: 31 October is last month's, unpaid, and 30 November is
/// today's, which counts in full however it was paid. It reaches back over a year too.
#[test]
fn on_the_last_of_a_month_its_own_bill_is_still_to_come() {
    let as_of = date(2026, 11, 30);
    let home = CategoryId::new();
    let rent = monthly("Rent", usd(1_000), date(2026, 1, 31), home);
    let source = rent.source;
    let forecast = run_as_given(&ForecastInput {
        today: as_of,
        as_of,
        stale_after_days: 365,
        items: vec![rent],
        ..one_month()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::BillUnpaidLastMonth {
            item: source,
            on: date(2026, 10, 31),
            amount: usd(1_000),
        }]
    );
    assert_eq!(line(&forecast, "Rent").unpaid, usd(1_000));

    let january = date(2027, 1, 15);
    let looking_over_the_year = run_as_given(&ForecastInput {
        today: january,
        as_of: january,
        items: vec![one_off("Tyres", usd(1_000), date(2026, 12, 1), home)],
        ..one_month()
    });
    assert_eq!(line(&looking_over_the_year, "Tyres").unpaid, usd(1_000));
}

/// A what-if owes nothing and takes nothing: an imagined bill of the same amount from last month
/// owes nothing for it, and the stored one keeps its payment. 1,000,000 less 5 December's 1,000 for
/// each of them.
#[test]
fn a_what_if_neither_owes_nor_takes_a_payment() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![
            monthly("Rent", usd(1_000), date(2026, 11, 5), home),
            hypothetical(0, monthly("Imagined", usd(1_000), date(2026, 10, 5), home)),
        ],
        spent_transactions: vec![paid_on(home, usd(1_000), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(line(&forecast, "Rent").unpaid, usd(0), "paid");
    let imagined = line(&forecast, "Imagined");
    assert_eq!(imagined.unpaid, usd(0));
    assert_eq!(imagined.payments, []);
    assert_eq!(forecast.surplus, Some(usd(998_000)));
    assert_eq!(forecast.cautions, []);
}

/// A paid bill that is past and has no more dates is shown for what paid it, but brings nothing to
/// the number: not even its currency, which would block the forecast.
#[test]
fn a_paid_bill_in_another_currency_blocks_nothing() {
    let home = CategoryId::new();
    let forecast = run_as_given(&ForecastInput {
        items: vec![one_off(
            "Abroad",
            super::eur(1_000),
            date(2026, 11, 5),
            home,
        )],
        spent_transactions: vec![paid_on(home, super::eur(1_000), date(2026, 11, 5))],
        ..one_month()
    });
    assert_eq!(forecast.blockers, []);
    assert_eq!(forecast.totals.len(), 1, "dollars only");
    assert_eq!(line(&forecast, "Abroad").counted, super::eur(0));
    assert_eq!(forecast.status, ForecastStatus::Firm);
}
