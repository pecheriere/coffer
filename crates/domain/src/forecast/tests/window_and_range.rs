//! The window's edges, its buckets, arithmetic limits and the largest household.

use jiff::civil::{Date, date};

use super::{cash, fixed_floor, household, inflow, line, one_off, run, stored, usd, usd_totals};
use super::{every, goal};
use crate::forecast::{ForecastError, ForecastInput, MonthBucket, compute_forecast};
use crate::ids::CategoryId;
use crate::plan::{
    CalendarMonth, IntervalUnit, MAX_HORIZON_MONTHS, MAX_PLAN_AMOUNT_MINOR, MAX_PLAN_ITEMS,
    SafetyFloor,
};

fn buckets(forecast: &[MonthBucket]) -> Vec<(String, bool)> {
    forecast
        .iter()
        .map(|bucket| (bucket.month.to_string(), bucket.partial))
        .collect()
}

fn window(as_of: Date, horizon_months: u8) -> ForecastInput {
    ForecastInput {
        today: as_of,
        as_of,
        horizon_months,
        ..household()
    }
}

#[test]
fn a_window_from_the_first_covers_whole_months() {
    let forecast = run(&window(date(2026, 11, 1), 2));
    assert_eq!(forecast.end_exclusive, date(2027, 1, 1), "two months on");
    assert_eq!(
        buckets(&forecast.months),
        [("2026-11".to_owned(), false), ("2026-12".to_owned(), false)],
        "January is not touched, and neither month is partial"
    );
}

#[test]
fn a_month_end_as_of_clamps() {
    let forecast = run(&window(date(2027, 1, 31), 1));
    assert_eq!(forecast.end_exclusive, date(2027, 2, 28), "February has 28");
    assert_eq!(
        buckets(&forecast.months),
        [("2027-01".to_owned(), true), ("2027-02".to_owned(), true)],
        "one day of January, then all but the last of February"
    );
}

#[test]
fn a_window_ending_on_the_second_touches_that_month() {
    let forecast = run(&window(date(2026, 11, 2), 1));
    assert_eq!(forecast.end_exclusive, date(2026, 12, 2), "one month on");
    assert_eq!(
        buckets(&forecast.months),
        [("2026-11".to_owned(), true), ("2026-12".to_owned(), true)],
        "December 1 is in the window"
    );
}

#[test]
fn the_window_includes_as_of_and_excludes_its_end() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            one_off("Yesterday", usd(1), date(2026, 11, 14), other),
            one_off("Today", usd(10), date(2026, 11, 15), other),
            one_off("Last", usd(100), date(2027, 2, 14), other),
            one_off("End", usd(1_000), date(2027, 2, 15), other),
        ],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).planned_outflows,
        usd(110),
        "the middle"
    );
}

#[test]
fn every_currency_has_a_line_even_with_nothing_planned() {
    let forecast = run(&household());
    for bucket in &forecast.months {
        let [only] = bucket.lines.as_slice() else {
            panic!("one line per currency in use");
        };
        assert_eq!(only.projected_cash, usd(1_000_000), "cash carries on");
        assert_eq!(only.items, [], "nothing planned");
    }
}

#[test]
fn the_end_of_the_calendar_does_not_hang() {
    let forecast = run(&window(date(9999, 12, 15), 3));
    assert_eq!(forecast.end_exclusive, Date::MAX, "as far as dates go");
    assert_eq!(
        buckets(&forecast.months),
        [("9999-12".to_owned(), true)],
        "one bucket"
    );
    let forecast = run(&window(Date::MAX, 3));
    assert_eq!(forecast.months, [], "an empty window");
}

#[test]
fn a_date_with_no_month_is_refused() {
    assert_eq!(
        compute_forecast(&window(date(-1, 1, 1), 1)),
        Err(ForecastError::OutOfRange),
        "before year 0"
    );
}

#[test]
fn totals_past_i64_are_out_of_range() {
    let other = CategoryId::new();
    let cases = [
        // Two occurrences of the largest number overflow one item's gross.
        ForecastInput {
            items: vec![stored(
                "Huge",
                usd(i64::MAX),
                every(1, IntervalUnit::Days, date(2026, 11, 15)),
            )],
            ..household()
        },
        // Each item fits, their sum does not.
        ForecastInput {
            items: vec![
                one_off("A", usd(i64::MAX), date(2026, 12, 1), other),
                one_off("B", usd(1), date(2026, 12, 1), other),
            ],
            ..household()
        },
        // Cash overflows, both ways.
        ForecastInput {
            accounts: vec![cash("A", usd(i64::MAX)), cash("B", usd(1))],
            ..household()
        },
        ForecastInput {
            accounts: vec![cash("A", usd(i64::MIN)), cash("B", usd(-1))],
            ..household()
        },
        // Each total fits, the need does not.
        ForecastInput {
            floor: Some(fixed_floor(usd(1))),
            items: vec![one_off("A", usd(i64::MAX), date(2026, 12, 1), other)],
            ..household()
        },
        // The need fits, the surplus does not.
        ForecastInput {
            accounts: vec![cash("A", usd(i64::MIN))],
            floor: Some(fixed_floor(usd(1))),
            ..household()
        },
        // Projected cash leaves i64 in a middle month.
        ForecastInput {
            include_income: true,
            accounts: vec![cash("A", usd(i64::MAX))],
            floor: Some(fixed_floor(usd(i64::MAX))),
            items: vec![
                inflow(one_off("In", usd(1), date(2026, 11, 20), other)),
                one_off("Out", usd(1), date(2026, 12, 20), other),
            ],
            ..household()
        },
        // Goals add up past it.
        ForecastInput {
            items: vec![
                goal("A", usd(i64::MAX), date(2026, 1, 1), date(2026, 12, 1), 0),
                goal("B", usd(i64::MAX), date(2026, 1, 1), date(2026, 12, 1), 0),
            ],
            ..household()
        },
    ];
    for input in cases {
        assert_eq!(
            compute_forecast(&input),
            Err(ForecastError::OutOfRange),
            "never a wrapped number"
        );
    }
    assert_eq!(
        ForecastError::OutOfRange.to_string(),
        "the forecast's totals are too large to represent",
        "a fixed message"
    );
}

#[test]
fn totals_at_the_limit_stay_exact() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("A", usd(i64::MAX))],
        items: vec![one_off("A", usd(i64::MAX), date(2026, 12, 1), other)],
        ..household()
    });
    let totals = usd_totals(&forecast);
    assert_eq!(totals.cash_needed, usd(i64::MAX), "right up to the limit");
    assert_eq!(totals.surplus, Some(usd(0)), "and exact");
    assert_eq!(line(&forecast, "A").counted, usd(i64::MAX), "on its line");

    // The sum passes through a value outside i64 on its way to one inside it.
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("A", usd(i64::MAX)),
            cash("B", usd(i64::MAX)),
            cash("C", usd(i64::MIN)),
        ],
        ..household()
    });
    assert_eq!(
        usd_totals(&forecast).cash_available,
        usd(i64::MAX - 1),
        "MAX + MAX + MIN is MAX - 1"
    );
}

fn most_items(amount: i64) -> ForecastInput {
    ForecastInput {
        horizon_months: MAX_HORIZON_MONTHS,
        floor: Some(SafetyFloor::MonthsOfOutflows {
            months: MAX_HORIZON_MONTHS,
        }),
        items: (0..MAX_PLAN_ITEMS)
            .map(|_| {
                stored(
                    "Daily",
                    usd(amount),
                    every(1, IntervalUnit::Days, date(2020, 1, 1)),
                )
            })
            .collect(),
        ..household()
    }
}

/// [2026-11-15, 2031-11-15) is 5 × 365 + 1 = 1,826 days: 913,000 dates in all.
#[test]
fn the_largest_household_stays_exact() {
    let forecast = run(&most_items(3));
    let totals = usd_totals(&forecast);
    assert_eq!(totals.planned_outflows, usd(2_739_000), "913,000 × 3");
    assert_eq!(
        totals.floor,
        usd(2_739_000),
        "the floor sums the same dates"
    );
    assert_eq!(forecast.months.len(), 61, "61 calendar months touched");
    assert_eq!(forecast.items.len(), MAX_PLAN_ITEMS, "one line per item");
    let first = forecast.months.first().unwrap().lines.first().unwrap();
    assert_eq!(first.outflows, usd(24_000), "16 days × 500 × 3");
    assert_eq!(first.items.len(), MAX_PLAN_ITEMS, "aggregated per item");
    let november_2031 = forecast.months.last().unwrap();
    assert_eq!(
        november_2031.month,
        "2031-11".parse::<CalendarMonth>().unwrap(),
        "the last month the window touches"
    );
    assert_eq!(
        november_2031.lines.first().unwrap().outflows,
        usd(21_000),
        "14 days × 500 × 3"
    );
}

#[test]
fn the_largest_amounts_are_refused_not_wrapped() {
    assert_eq!(
        compute_forecast(&most_items(MAX_PLAN_AMOUNT_MINOR)),
        Err(ForecastError::OutOfRange),
        "913,000 × 10^15 passes i64"
    );
}

/// API clients read these names; each kind of enum keeps its own shape.
#[test]
fn json_shapes_are_stable() {
    let forecast = run(&ForecastInput {
        accounts: Vec::new(),
        floor: None,
        ..household()
    });
    let json = serde_json::to_value(&forecast).unwrap();
    assert_eq!(json["status"], "unavailable", "a status is plain text");
    assert!(json["surplus"].is_null(), "no number is an explicit null");
    assert_eq!(
        json["blockers"],
        serde_json::json!([{"kind": "floor_not_set"}, {"kind": "no_counted_accounts"}]),
        "blockers are tagged"
    );

    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        today: date(2026, 11, 1),
        accounts: vec![crate::forecast::AccountInput {
            closed: true,
            ..cash("Old", usd(1))
        }],
        items: vec![super::hypothetical(
            7,
            one_off("What if", usd(5), date(2026, 12, 1), other),
        )],
        ..household()
    });
    let json = serde_json::to_value(&forecast).unwrap();
    assert_eq!(
        json["cautions"][0],
        serde_json::json!({"kind": "as_of_not_today", "today": "2026-11-01"}),
        "cautions are tagged"
    );
    assert_eq!(
        json["accounts"][0]["not_counted"], "closed",
        "a reason is plain text"
    );
    assert_eq!(
        json["items"][0]["source"],
        serde_json::json!({"kind": "hypothetical", "index": 7}),
        "a source is tagged"
    );
    assert_eq!(json["months"][0]["month"], "2026-11", "a month is YYYY-MM");
    assert_eq!(json["items"][0]["item_type"], "one_off", "the item's type");
    assert_eq!(json["items"][0]["direction"], "outflow", "its direction");
}
