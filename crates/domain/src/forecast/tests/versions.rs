//! An item with a history is given as one input per version (spec §10.3), and counts as one
//! item. Worked by hand from the dates and amounts here, as of 15 November 2026.

use std::collections::BTreeMap;

use jiff::civil::date;
use proptest::prelude::{Strategy, prop_assert_eq};
use proptest::{collection, proptest};

use super::{
    AS_OF, budget, cash, every, household, monthly, paid_on, run_as_given, spent, usd, usd_totals,
};
use crate::forecast::{Caution, Forecast, ForecastInput, ItemInput, ItemLine, compute_forecast};
use crate::ids::CategoryId;
use crate::plan::{CalendarMonth, IntervalUnit, MonthSpan};

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

fn before(item: &ItemInput, until: &str) -> ItemInput {
    ItemInput {
        in_force: MonthSpan {
            from: None,
            until: Some(month(until)),
        },
        ..item.clone()
    }
}

/// `item` as it reads from `from`, at `minor` dollars.
fn from(item: &ItemInput, from: &str, minor: i64) -> ItemInput {
    ItemInput {
        amount: usd(minor),
        in_force: MonthSpan {
            from: Some(month(from)),
            until: None,
        },
        ..item.clone()
    }
}

fn lines<'a>(forecast: &'a Forecast, name: &str) -> Vec<&'a ItemLine> {
    forecast
        .items
        .iter()
        .filter(|line| line.name == name)
        .collect()
}

fn rent(home: CategoryId) -> ItemInput {
    monthly("Rent", usd(1_000), date(2026, 1, 5), home)
}

/// A rent of 1,000 lowered to 800 from this month, paid 1,000 on the 5th of last month and
/// of this one.
fn lowered_and_paid(home: CategoryId) -> ForecastInput {
    let old = rent(home);
    let new = ItemInput {
        also_matches: BTreeMap::from([(month("2026-11"), vec![usd(1_000)])]),
        ..from(&old, "2026-11", 800)
    };
    ForecastInput {
        items: vec![before(&old, "2026-11"), new],
        spent_transactions: vec![
            paid_on(home, usd(1_000), date(2026, 10, 5)),
            paid_on(home, usd(1_000), date(2026, 11, 5)),
        ],
        ..household()
    }
}

#[test]
fn a_paid_bill_lowered_this_month_stays_paid() {
    let home = CategoryId::new();
    let forecast = run_as_given(&lowered_and_paid(home));

    let [old, new] = lines(&forecast, "Rent").try_into().unwrap();
    assert_eq!(old.effective_from, None);
    assert_eq!(new.effective_from, Some(month("2026-11")));
    let november = new
        .payments
        .iter()
        .find(|payment| payment.on == date(2026, 11, 5))
        .unwrap();
    let paid = november.paid_by.expect("the 1,000 paid the 800 bill");
    assert_eq!(paid.difference, usd(200));
    assert_eq!(old.unpaid, usd(0));
    assert_eq!(new.unpaid, usd(0));
    assert!(
        !forecast.cautions.iter().any(|caution| matches!(
            caution,
            Caution::BillUnpaidLastMonth { .. } | Caution::BillNotSeen { .. }
        )),
        "{:?}",
        forecast.cautions
    );
    // December, January and February, at 800.
    assert_eq!(new.counted, usd(2_400));
}

#[test]
fn without_the_old_amount_the_lowered_bill_is_not_paid() {
    let home = CategoryId::new();
    let mut input = lowered_and_paid(home);
    input.items[1].also_matches.clear();
    let forecast = run_as_given(&input);
    assert!(
        forecast
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::BillNotSeen { .. })),
        "1,000 is not within 10% of 800"
    );
}

#[test]
fn two_spans_are_two_lines_and_one_item() {
    let home = CategoryId::new();
    let old = rent(home);
    let forecast = run_as_given(&ForecastInput {
        items: vec![before(&old, "2027-01"), from(&old, "2027-01", 1_200)],
        spent_transactions: vec![
            paid_on(home, usd(1_000), date(2026, 10, 5)),
            paid_on(home, usd(1_000), date(2026, 11, 5)),
        ],
        ..household()
    });

    let [first, second] = lines(&forecast, "Rent").try_into().unwrap();
    assert_eq!(
        (first.effective_from, second.effective_from),
        (None, Some(month("2027-01")))
    );
    // Rent falls on the 5th: from 15 November the window holds 5 December, January and
    // February, and only December is before the rise in January.
    assert_eq!(first.gross, usd(1_000));
    assert_eq!(second.gross, usd(2_400));
    assert_eq!(usd_totals(&forecast).planned_outflows, usd(3_400));
}

#[test]
fn a_category_change_moves_the_budget_pool() {
    let groceries = CategoryId::new();
    let household_goods = CategoryId::new();
    let old = budget(monthly("Food", usd(300), date(2026, 1, 10), groceries));
    let new = ItemInput {
        category: Some(household_goods),
        ..from(&old, "2026-12", 300)
    };
    let forecast = run_as_given(&ForecastInput {
        items: vec![before(&old, "2026-12"), new],
        category_spent: vec![spent(groceries, 100), spent(household_goods, 40)],
        ..household()
    });

    let [first, second] = lines(&forecast, "Food").try_into().unwrap();
    // 10 November is the one occurrence before the change, and groceries took 100 of it.
    assert_eq!(
        (first.gross, first.reduced_by, first.counted),
        (usd(300), usd(100), usd(200))
    );
    // Household goods spending this month falls outside the span, so it reduces nothing.
    assert_eq!((second.gross, second.reduced_by), (usd(900), usd(0)));
}

#[test]
fn an_items_cautions_are_raised_once() {
    let home = CategoryId::new();
    let mut input = household();
    let mut elsewhere = cash("Elsewhere", usd(5_000));
    elsewhere.include_in_forecast = false;
    let hinted = ItemInput {
        account_hint: Some(elsewhere.id),
        ..rent(home)
    };
    input.accounts.push(elsewhere);
    input.items = vec![before(&hinted, "2026-12"), from(&hinted, "2026-12", 1_200)];
    input.spent_transactions = vec![
        paid_on(home, usd(1_000), date(2026, 10, 5)),
        paid_on(home, usd(1_000), date(2026, 11, 5)),
    ];
    let forecast = run_as_given(&input);

    let hints = forecast
        .cautions
        .iter()
        .filter(|caution| matches!(caution, Caution::ItemHintNotCounted { .. }))
        .count();
    assert_eq!(hints, 1);
}

#[test]
fn a_span_ended_last_month_still_answers_for_last_month() {
    let home = CategoryId::new();
    let old = rent(home);
    let forecast = run_as_given(&ForecastInput {
        items: vec![before(&old, "2026-11"), from(&old, "2026-11", 800)],
        spent_transactions: vec![paid_on(home, usd(800), date(2026, 11, 5))],
        ..household()
    });

    let [first, second] = lines(&forecast, "Rent").try_into().unwrap();
    assert_eq!(first.unpaid, usd(1_000), "October's rent at its own amount");
    assert_eq!(second.unpaid, usd(0));
    assert!(forecast.cautions.iter().any(|caution| matches!(
        caution,
        Caution::BillUnpaidLastMonth { item, .. } if *item == first.source
    )));
}

/// A recurring item with its monthly, weekly or daily rhythm.
fn rhythm() -> impl Strategy<Value = (u16, IntervalUnit, i8, i64, i64, u8)> {
    (
        1_u16..4,
        proptest::sample::select(vec![
            IntervalUnit::Days,
            IntervalUnit::Weeks,
            IntervalUnit::Months,
        ]),
        1_i8..=28,
        1_i64..100_000,
        1_i64..100_000,
        0_u8..3,
    )
}

proptest! {
    /// Two spans of one item add up to two items whose schedules are cut at the month.
    #[test]
    fn spans_add_up_like_clipped_items(
        rows in collection::vec(rhythm(), 1..4),
    ) {
        let mut as_spans = household();
        let mut as_items = household();
        for (every_n, unit, day, first, second, split_offset) in rows {
            let category = CategoryId::new();
            let anchor = date(2026, 8, day);
            let split = (0..split_offset).fold(month("2026-11"), |m, _| m.next());
            let whole = ItemInput {
                category: Some(category),
                schedule: every(every_n, unit, anchor),
                amount: usd(first),
                ..monthly("Item", usd(first), anchor, category)
            };
            as_spans.items.push(before(&whole, &split.to_string()));
            as_spans.items.push(from(&whole, &split.to_string(), second));

            // The same two items with schedules cut at the split: the first ends the day
            // before it, the second starts at its first occurrence from it.
            let cut = split.first_day();
            let last_day = cut.yesterday().unwrap();
            let mut early = whole.clone();
            early.schedule = match early.schedule {
                crate::plan::PlanSchedule::Recurring { every, unit, anchor_on, .. } => {
                    crate::plan::PlanSchedule::Recurring { every, unit, anchor_on, end_on: Some(last_day) }
                }
                other => other,
            };
            let first_after = whole.schedule.occurrences(cut, date(2030, 1, 1)).first().copied();
            let mut late = ItemInput { amount: usd(second), ..whole.clone() };
            if let Some(anchor_on) = first_after {
                late.schedule = every(every_n, unit, anchor_on);
                as_items.items.push(late);
            }
            as_items.items.push(early);
        }
        let spans = compute_forecast(&as_spans).unwrap();
        let items = compute_forecast(&as_items).unwrap();
        prop_assert_eq!(spans.totals, items.totals);
        prop_assert_eq!(AS_OF, as_spans.as_of);
    }
}
