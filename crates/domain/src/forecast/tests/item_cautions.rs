//! R4: what a planned item raises, which keeps the number but stops it being firm.

use jiff::civil::date;

use super::{assert_provisional, cash, goal, household, inflow};
use super::{line, one_off, run, usd, usd_totals};
use crate::forecast::{
    AccountInput, Caution, ForecastInput, ForecastStatus, HintEffect, ItemInput,
};
use crate::ids::{AccountId, CategoryId};

fn car_goal(category: CategoryId, due_on: jiff::civil::Date) -> ItemInput {
    ItemInput {
        category: Some(category),
        ..goal("Car fund", usd(50_000), date(2026, 1, 1), due_on, 0)
    }
}

#[test]
fn goal_one_off_overlap_warns_and_counts_both() {
    let car = CategoryId::new();
    let fund = car_goal(car, date(2026, 12, 31));
    let purchase = one_off("Car", usd(50_000), date(2027, 1, 15), car);
    let (goal_source, one_off_source) = (fund.source, purchase.source);
    let forecast = run(&ForecastInput {
        items: vec![fund, purchase],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::GoalOneOffOverlap {
            goal: goal_source,
            one_off: one_off_source,
            category: car,
        }],
        "they may be the same expense"
    );
    assert_provisional(&forecast);
    assert_eq!(usd_totals(&forecast).goals, usd(50_000), "the goal counts");
    assert_eq!(
        usd_totals(&forecast).planned_outflows,
        usd(50_000),
        "and so does the one-off"
    );
}

#[test]
fn overlap_reaches_exactly_one_calendar_month() {
    let car = CategoryId::new();
    let due_on = date(2026, 12, 15);
    for (on, warns) in [
        (date(2026, 11, 15), true),
        (date(2026, 11, 14), false),
        (date(2027, 1, 15), true),
        (date(2027, 1, 16), false),
    ] {
        let forecast = run(&ForecastInput {
            items: vec![car_goal(car, due_on), one_off("Car", usd(1), on, car)],
            ..household()
        });
        assert_eq!(
            forecast
                .cautions
                .iter()
                .any(|caution| matches!(caution, Caution::GoalOneOffOverlap { .. })),
            warns,
            "a calendar month either side, inclusive"
        );
    }
}

#[test]
fn overlap_outside_a_month_does_not_warn() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            car_goal(car, date(2026, 12, 15)),
            one_off("Car", usd(1), date(2027, 1, 20), car),
        ],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "too far apart");
    assert_eq!(forecast.status, ForecastStatus::Firm, "so firm");
}

#[test]
fn overlap_needs_the_same_category() {
    let forecast = run(&ForecastInput {
        items: vec![
            car_goal(CategoryId::new(), date(2026, 12, 15)),
            one_off("Car", usd(1), date(2026, 12, 15), CategoryId::new()),
            ItemInput {
                category: None,
                ..one_off("None", usd(1), date(2026, 12, 15), CategoryId::new())
            },
        ],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "different categories");
}

#[test]
fn overlap_ignores_goals_that_count_nothing_and_inflows() {
    let car = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![
            ItemInput {
                goal_spent_minor: 50_000,
                ..car_goal(car, date(2026, 12, 15))
            },
            one_off("Car", usd(1), date(2026, 12, 15), car),
        ],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "a spent goal counts nothing");

    let forecast = run(&ForecastInput {
        items: vec![
            car_goal(car, date(2026, 12, 15)),
            inflow(one_off("Sale", usd(1), date(2026, 12, 15), car)),
        ],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "an inflow is not the same expense");
}

#[test]
fn overdue_goal_counts_full_remaining_and_warns() {
    let fund = goal(
        "Late fund",
        usd(50_000),
        date(2026, 1, 1),
        date(2026, 10, 1),
        20_000,
    );
    let source = fund.source;
    let forecast = run(&ForecastInput {
        items: vec![fund],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::GoalOverdue {
            goal: source,
            due_on: date(2026, 10, 1),
        }],
        "the date has passed"
    );
    assert_provisional(&forecast);
    let line = line(&forecast, "Late fund");
    assert_eq!(line.counted, usd(30_000), "all that remains");
    assert!(!line.goal.unwrap().due_in_horizon, "it is already past");
    let accruals: Vec<_> = forecast
        .months
        .iter()
        .map(|bucket| bucket.lines.first().unwrap().goal_accrual)
        .collect();
    assert_eq!(
        accruals,
        [usd(30_000), usd(0), usd(0), usd(0)],
        "all of it in the first month"
    );
}

#[test]
fn item_hinted_at_uncounted_account_counts_and_warns() {
    let side = AccountInput {
        include_in_forecast: false,
        ..cash("Side", usd(100))
    };
    let side_id = side.id;
    let bill = ItemInput {
        account_hint: Some(side_id),
        ..one_off("Bill", usd(10_000), date(2026, 12, 1), CategoryId::new())
    };
    let source = bill.source;
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(1_000_000)), side],
        items: vec![bill],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::ItemHintNotCounted {
            item: source,
            account: side_id,
            effect: HintEffect::StillNeeded,
        }],
        "the need may be overstated"
    );
    assert_provisional(&forecast);
    assert_eq!(
        forecast.surplus,
        Some(usd(990_000)),
        "the bill still counts"
    );
}

#[test]
fn hints_that_change_nothing_do_not_warn() {
    let checking = cash("Checking", usd(1_000_000));
    let counted = checking.id;
    let elsewhere = AccountId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![checking],
        items: vec![
            ItemInput {
                account_hint: Some(counted),
                ..one_off("Counted", usd(1), date(2026, 12, 1), CategoryId::new())
            },
            ItemInput {
                account_hint: Some(elsewhere),
                ..one_off("Outside", usd(1), date(2030, 1, 1), CategoryId::new())
            },
            inflow(ItemInput {
                account_hint: Some(elsewhere),
                ..one_off("Excluded", usd(1), date(2026, 12, 1), CategoryId::new())
            }),
        ],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [],
        "a counted hint, an item outside the window, income left out"
    );
}

fn linked_both_sides(completed: bool) -> ItemInput {
    let mut item = ItemInput {
        goal_linked_both_sides: true,
        ..goal(
            "Tuition",
            usd(1_000),
            date(2026, 1, 1),
            date(2027, 1, 1),
            1_000,
        )
    };
    item.schedule = crate::plan::PlanSchedule::Goal {
        start_on: date(2026, 1, 1),
        due_on: date(2027, 1, 1),
        completed,
    };
    item
}

/// Fully spent, so it has no line, which is exactly when the links may have overstated it.
#[test]
fn a_goal_linked_both_sides_warns_with_no_line_of_its_own() {
    let tuition = linked_both_sides(false);
    let source = tuition.source;
    let forecast = run(&ForecastInput {
        items: vec![tuition],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::GoalLinkedBothSides { goal: source }]
    );
    assert_provisional(&forecast);
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "the number stays");
}

#[test]
fn a_finished_goal_linked_both_sides_raises_nothing() {
    let forecast = run(&ForecastInput {
        items: vec![linked_both_sides(true)],
        ..household()
    });
    assert_eq!(forecast.cautions, []);
    assert_eq!(forecast.status, ForecastStatus::Firm);
}

/// `item` hinted at `account`, beside a counted Checking holding 1,000,000, with income on.
fn hinted_at(account: AccountInput, item: ItemInput) -> crate::forecast::Forecast {
    let id = account.id;
    run(&ForecastInput {
        include_income: true,
        accounts: vec![cash("Checking", usd(1_000_000)), account],
        items: vec![ItemInput {
            account_hint: Some(id),
            ..item
        }],
        ..household()
    })
}

fn held_fund() -> AccountInput {
    AccountInput {
        belongs_to: crate::accounts::BelongsTo::SomeoneElse,
        include_in_forecast: false,
        ..cash("Fund", usd(0))
    }
}

fn gift(amount: crate::money::Money) -> ItemInput {
    inflow(one_off(
        "Gift",
        amount,
        date(2026, 12, 1),
        CategoryId::new(),
    ))
}

#[test]
fn an_inflow_hinted_at_a_held_account_is_left_out() {
    let forecast = hinted_at(held_fund(), gift(usd(10_000)));

    assert_eq!(forecast.surplus, Some(usd(1_000_000)));
    assert_eq!(usd_totals(&forecast).excluded_inflows, usd(0));
    assert_eq!(line(&forecast, "Gift").counted, usd(0));
    assert!(matches!(
        forecast.cautions.as_slice(),
        [Caution::ItemHintNotCounted {
            effect: HintEffect::LeftOut,
            ..
        }]
    ));
}

/// Its currency isn't in use: nothing of it reaches the number.
#[test]
fn an_inflow_in_another_currency_hinted_at_a_held_account_mixes_nothing() {
    let forecast = hinted_at(held_fund(), gift(super::eur(10_000)));

    assert_eq!(forecast.blockers, []);
    assert_eq!(forecast.totals.len(), 1);
    assert_eq!(forecast.surplus, Some(usd(1_000_000)));
}

/// The household's own account switched out of the forecast: the money may never reach counted
/// cash, so it stays out, as it does for a held account. The surplus is what it is without the
/// inflow, and the caution says why it reads low.
#[test]
fn an_inflow_hinted_at_an_excluded_household_account_is_left_out() {
    let side = AccountInput {
        include_in_forecast: false,
        ..cash("Side", usd(0))
    };
    let forecast = hinted_at(side, gift(usd(10_000)));

    assert_eq!(forecast.surplus, Some(usd(1_000_000)));
    assert_eq!(usd_totals(&forecast).planned_inflows, usd(0));
    assert_eq!(usd_totals(&forecast).excluded_inflows, usd(0));
    assert_eq!(line(&forecast, "Gift").counted, usd(0));
    assert!(matches!(
        forecast.cautions.as_slice(),
        [Caution::ItemHintNotCounted {
            effect: HintEffect::LeftOut,
            ..
        }]
    ));
}

/// Hinting an inflow at an account that stays out never changes the surplus: it is the same
/// number as for an inflow that was never planned.
#[test]
fn a_hint_at_an_excluded_account_leaves_the_surplus_unchanged_with_income_on() {
    let side = AccountInput {
        include_in_forecast: false,
        ..cash("Side", usd(0))
    };
    let without = run(&ForecastInput {
        include_income: true,
        accounts: vec![cash("Checking", usd(1_000_000)), side.clone()],
        ..household()
    });
    let hinted = hinted_at(side, gift(usd(10_000)));

    assert_eq!(hinted.surplus, without.surplus);
}

#[test]
fn an_inflow_hinted_at_a_counted_account_counts() {
    let savings = cash("Savings", usd(0));
    let forecast = hinted_at(savings, gift(usd(10_000)));

    assert_eq!(forecast.surplus, Some(usd(1_010_000)));
    assert_eq!(forecast.cautions, []);
}

#[test]
fn an_inflow_with_no_hint_counts() {
    let forecast = run(&ForecastInput {
        include_income: true,
        items: vec![gift(usd(10_000))],
        ..household()
    });

    assert_eq!(forecast.surplus, Some(usd(1_010_000)));
    assert_eq!(forecast.cautions, []);
}

#[test]
fn an_outflow_hinted_at_a_held_account_still_counts() {
    let forecast = hinted_at(
        held_fund(),
        one_off("Bill", usd(10_000), date(2026, 12, 1), CategoryId::new()),
    );

    assert_eq!(forecast.surplus, Some(usd(990_000)));
    assert!(matches!(
        forecast.cautions.as_slice(),
        [Caution::ItemHintNotCounted {
            effect: HintEffect::StillNeeded,
            ..
        }]
    ));
}
