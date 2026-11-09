use jiff::civil::{Date, date};
use proptest::prelude::{Strategy, prop_assert_eq};
use proptest::{collection, proptest};

use super::{EffectiveFrom, MonthSpan, PlanValues, PlanVersion, PlanVersions};
use crate::ids::CategoryId;
use crate::money::{Currency, Money};
use crate::plan::{CalendarMonth, PlanDirection, PlanError, PlanItemType, PlanTier};

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

fn values(category: CategoryId, minor: i64) -> PlanValues {
    PlanValues {
        amount: Money {
            minor_units: minor,
            currency: Currency::parse("USD").unwrap(),
        },
        direction: PlanDirection::Outflow,
        category,
        spending: None,
        tier: Some(PlanTier::MustPay),
    }
}

fn row(seq: u32, from: &str, written_on: Date, values: PlanValues) -> PlanVersion {
    PlanVersion {
        seq,
        effective_from: month(from),
        written_on,
        values,
    }
}

fn minor(values: &PlanValues) -> i64 {
    values.amount.minor_units
}

/// Three rows written in order: 1,000 from January, 1,200 from October, 900 from November.
fn history(category: CategoryId) -> PlanVersions {
    PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-10", date(2026, 10, 5), values(category, 1200)),
        row(3, "2026-11", date(2026, 11, 10), values(category, 900)),
    ])
    .unwrap()
}

#[test]
fn the_latest_written_row_that_has_started_is_in_force() {
    let category = CategoryId::new();
    let versions = PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-12", date(2026, 11, 5), values(category, 1200)),
        row(3, "2026-11", date(2026, 11, 10), values(category, 900)),
    ])
    .unwrap();
    assert_eq!(minor(versions.in_force(month("2026-10"))), 1000);
    assert_eq!(minor(versions.in_force(month("2026-11"))), 900);
    assert_eq!(
        minor(versions.in_force(month("2026-12"))),
        900,
        "C replaced the change planned for December"
    );
    let swapped = PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-11", date(2026, 11, 5), values(category, 900)),
        row(3, "2026-12", date(2026, 11, 10), values(category, 1200)),
    ])
    .unwrap();
    assert_eq!(minor(swapped.in_force(month("2026-11"))), 900);
    assert_eq!(minor(swapped.in_force(month("2026-12"))), 1200);
}

#[test]
fn before_the_first_row_the_first_is_in_force() {
    let versions = history(CategoryId::new());
    assert_eq!(minor(versions.in_force(month("2025-06"))), 1000);
}

#[test]
fn timeline_drops_a_row_never_in_force() {
    let category = CategoryId::new();
    let versions = PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-12", date(2026, 11, 5), values(category, 1200)),
        row(3, "2026-11", date(2026, 11, 10), values(category, 900)),
    ])
    .unwrap();
    let timeline = versions.timeline();
    assert_eq!(timeline.len(), 2, "B never reads");
    assert_eq!(timeline[0].version.seq, 1);
    assert_eq!(
        timeline[0].span,
        MonthSpan {
            from: None,
            until: Some(month("2026-11"))
        }
    );
    assert_eq!(timeline[1].version.seq, 3);
    assert_eq!(
        timeline[1].span,
        MonthSpan {
            from: Some(month("2026-11")),
            until: None
        }
    );
}

#[test]
fn held_during_keeps_the_amount_a_month_opened_with() {
    let category = CategoryId::new();
    let versions = PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-11", date(2026, 11, 10), values(category, 800)),
    ])
    .unwrap();
    let november: Vec<i64> = versions
        .held_during(month("2026-11"))
        .into_iter()
        .map(minor)
        .collect();
    assert_eq!(november, vec![1000, 800]);
    let december: Vec<i64> = versions
        .held_during(month("2026-12"))
        .into_iter()
        .map(minor)
        .collect();
    assert_eq!(december, vec![800]);

    let replaced = PlanVersions::new(vec![
        row(1, "2026-01", date(2026, 1, 3), values(category, 1000)),
        row(2, "2026-11", date(2026, 10, 5), values(category, 700)),
        row(3, "2026-11", date(2026, 10, 20), values(category, 800)),
    ])
    .unwrap();
    let november: Vec<i64> = replaced
        .held_during(month("2026-11"))
        .into_iter()
        .map(minor)
        .collect();
    assert_eq!(
        november,
        vec![800],
        "a row planned and replaced in October is never held in November"
    );
}

#[test]
fn versions_must_be_numbered_in_order() {
    assert_eq!(PlanVersions::new(Vec::new()), Err(PlanError::EmptyVersions));
    let category = CategoryId::new();
    let a = row(2, "2026-01", date(2026, 1, 3), values(category, 1));
    let b = row(2, "2026-02", date(2026, 2, 3), values(category, 2));
    assert_eq!(
        PlanVersions::new(vec![a, b]),
        Err(PlanError::UnorderedVersions)
    );
}

#[test]
fn resolve_refuses_the_past_and_next_month_for_non_recurring() {
    let today = date(2026, 11, 30);
    let recurring = PlanItemType::Recurring;
    assert_eq!(
        EffectiveFrom::Month(month("2026-10")).resolve(today, recurring),
        Err(PlanError::PastVersion)
    );
    assert_eq!(
        EffectiveFrom::Month(month("2027-01")).resolve(today, recurring),
        Err(PlanError::PastVersion)
    );
    for item_type in [PlanItemType::Goal, PlanItemType::OneOff] {
        assert_eq!(
            EffectiveFrom::NextMonth.resolve(today, item_type),
            Err(PlanError::NextMonthNeedsRecurring)
        );
        assert_eq!(
            EffectiveFrom::ThisMonth.resolve(today, item_type),
            Ok(month("2026-11"))
        );
    }
    assert_eq!(
        EffectiveFrom::Month(month("2026-12")).resolve(today, recurring),
        Ok(month("2026-12"))
    );
    assert_eq!(
        EffectiveFrom::NextMonth.resolve(today, recurring),
        Ok(month("2026-12"))
    );
}

#[test]
fn effective_from_parses_keywords_and_months() {
    let parse = |text: &str| serde_json::from_str::<EffectiveFrom>(text);
    assert_eq!(parse("\"this_month\"").unwrap(), EffectiveFrom::ThisMonth);
    assert_eq!(parse("\"next_month\"").unwrap(), EffectiveFrom::NextMonth);
    assert_eq!(
        parse("\"2026-12\"").unwrap(),
        EffectiveFrom::Month(month("2026-12"))
    );
    assert!(parse("\"soon\"").is_err());
    assert_eq!(
        serde_json::to_string(&EffectiveFrom::Month(month("2026-12"))).unwrap(),
        "\"2026-12\""
    );
}

#[test]
fn spans_know_their_months() {
    let span = MonthSpan {
        from: Some(month("2026-11")),
        until: Some(month("2027-01")),
    };
    assert!(span.contains(date(2026, 11, 1)));
    assert!(span.contains(date(2026, 12, 31)));
    assert!(!span.contains(date(2026, 10, 31)));
    assert!(!span.contains(date(2027, 1, 1)));
    assert!(MonthSpan::ALWAYS.contains(date(1999, 1, 1)));
    assert!(span.overlaps(MonthSpan::ALWAYS));
    let after = MonthSpan {
        from: Some(month("2027-01")),
        until: None,
    };
    assert!(!span.overlaps(after), "until is exclusive");
    let touching = MonthSpan {
        from: Some(month("2026-12")),
        until: None,
    };
    assert!(span.overlaps(touching));
}

/// A write made in month `offset` (from a base month), effective this month or next.
fn write_strategy() -> impl Strategy<Value = Vec<(u8, bool, i64)>> {
    collection::vec((0_u8..3, proptest::bool::ANY, 1_i64..10_000), 1..12)
}

proptest! {
    #[test]
    fn writes_never_change_a_past_month(writes in write_strategy()) {
        let category = CategoryId::new();
        let base = month("2026-01");
        let mut rows = vec![row(1, "2026-01", date(2026, 1, 1), values(category, 1))];
        let mut current = 0_i32;
        for (step, next, amount) in writes {
            current += i32::from(step);
            let this = (0..current).fold(base, |m, _| m.next());
            let before = PlanVersions::new(rows.clone()).unwrap();
            let seen: Vec<(CalendarMonth, i64)> = (0..current)
                .scan(base, |m, _| { let now = *m; *m = m.next(); Some(now) })
                .map(|m| (m, minor(before.in_force(m))))
                .collect();
            let effective = if next { this.next() } else { this };
            let seq = before.next_seq();
            rows.push(PlanVersion {
                seq,
                effective_from: effective,
                written_on: this.first_day(),
                values: values(category, amount),
            });
            let after = PlanVersions::new(rows.clone()).unwrap();
            for (m, was) in seen {
                prop_assert_eq!(minor(after.in_force(m)), was);
            }
        }
    }
}
