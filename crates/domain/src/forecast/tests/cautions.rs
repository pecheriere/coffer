//! R4: what keeps the number but stops it being firm. Each asserts the number is
//! still present.

use jiff::civil::date;

use super::{AS_OF, assert_provisional, cash, days_before, fixed_floor, goal, household, inflow};
use super::{currency, eur, line, monthly, one_off, run, usd, usd_totals};
use crate::accounts::LiquidityClass;
use crate::forecast::{
    AccountInput, BalanceInput, CategorySpent, Caution, FeedAlert, FeedProblem, FeedProblemReason,
    FeedStatus, ForecastInput, ForecastStatus, ItemInput, LooseEnds, UnboundAccount,
};
use crate::ids::{AccountId, CategoryId, ConnectionId, ProviderAccountId};
use crate::plan::{CalendarMonth, SafetyFloor};

fn aged(days: i32) -> AccountInput {
    AccountInput {
        balance: Some(BalanceInput {
            on: days_before(AS_OF, days),
            minor_units: 1_000,
        }),
        ..cash("Checking", usd(0))
    }
}

#[test]
fn stale_after_threshold_not_on_it() {
    let on_it = run(&ForecastInput {
        accounts: vec![aged(7)],
        ..household()
    });
    assert_eq!(on_it.cautions, [], "seven days is not yet stale");
    assert_eq!(on_it.status, ForecastStatus::Firm, "firm on the threshold");

    let account = aged(8);
    let id = account.id;
    let past_it = run(&ForecastInput {
        accounts: vec![account],
        ..household()
    });
    assert_eq!(
        past_it.cautions,
        [Caution::BalanceStale {
            account: id,
            on: date(2026, 11, 7),
            age_days: 8,
        }],
        "eight days is stale"
    );
    assert_provisional(&past_it);
    assert_eq!(
        past_it.surplus,
        Some(usd(1_000)),
        "the stale balance counts"
    );
}

#[test]
fn staleness_is_measured_at_as_of() {
    let forecast = run(&ForecastInput {
        today: date(2027, 1, 1),
        accounts: vec![aged(1)],
        ..household()
    });
    assert!(
        !forecast
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::BalanceStale { .. })),
        "the balance was fresh for the day the forecast describes"
    );
}

#[test]
fn an_uncounted_account_is_never_stale() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            aged(1),
            AccountInput {
                include_in_forecast: false,
                ..aged(90)
            },
        ],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "it changes nothing");
}

#[test]
fn unbound_account_reported_with_currency_support() {
    let connection = ConnectionId::new();
    let known = ProviderAccountId::new();
    let unknown = ProviderAccountId::new();
    let forecast = run(&ForecastInput {
        unbound: vec![
            UnboundAccount {
                provider_account: known,
                connection,
                name: "Spare".to_owned(),
                institution: Some("Example Bank".to_owned()),
                currency_raw: "USD".to_owned(),
            },
            UnboundAccount {
                provider_account: unknown,
                connection,
                name: "Rewards".to_owned(),
                institution: None,
                currency_raw: "POINTS".to_owned(),
            },
        ],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [
            Caution::UnboundProviderAccount {
                provider_account: known,
                connection,
                name: "Spare".to_owned(),
                institution: Some("Example Bank".to_owned()),
                currency_raw: "USD".to_owned(),
                currency_supported: true,
            },
            Caution::UnboundProviderAccount {
                provider_account: unknown,
                connection,
                name: "Rewards".to_owned(),
                institution: None,
                currency_raw: "POINTS".to_owned(),
                currency_supported: false,
            },
        ],
        "each says whether its currency could be supported"
    );
    assert_provisional(&forecast);
}

#[test]
fn feed_problem_reported() {
    let provider_account = ProviderAccountId::new();
    let account = AccountInput {
        feed_problems: vec![FeedProblem {
            provider_account,
            reason: FeedProblemReason::ProviderError,
        }],
        ..cash("Checking", usd(1_000))
    };
    let id = account.id;
    let forecast = run(&ForecastInput {
        accounts: vec![account],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::AccountFeedProblem {
            account: id,
            provider_account,
            reason: FeedProblemReason::ProviderError,
        }],
        "names the account and the reason"
    );
    assert_provisional(&forecast);
}

#[test]
fn feed_problem_on_an_uncounted_account_changes_nothing() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(1_000)),
            AccountInput {
                include_in_forecast: false,
                feed_problems: vec![FeedProblem {
                    provider_account: ProviderAccountId::new(),
                    reason: FeedProblemReason::CurrencyDiffers,
                }],
                ..cash("Side", usd(1))
            },
        ],
        ..household()
    });
    assert_eq!(
        forecast.status,
        ForecastStatus::Firm,
        "it is not in the sum"
    );
}

#[test]
fn feed_alert_reported() {
    let connection = ConnectionId::new();
    let forecast = run(&ForecastInput {
        feeds: vec![FeedAlert {
            connection,
            label: "Main bank".to_owned(),
            status: FeedStatus::ReauthRequired,
        }],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::ConnectionNeedsAttention {
            connection,
            label: "Main bank".to_owned(),
            status: FeedStatus::ReauthRequired,
        }],
        "names the connection"
    );
    assert_provisional(&forecast);
}

#[test]
fn as_of_other_than_today_is_a_caution() {
    let other = CategoryId::new();
    let today = date(2026, 10, 20);
    let forecast = run(&ForecastInput {
        today,
        items: vec![
            one_off("Before", usd(10_000), date(2026, 10, 25), other),
            one_off("After", usd(1_000), date(2026, 11, 20), other),
        ],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::AsOfNotToday { today }],
        "the gap is not hidden"
    );
    assert_provisional(&forecast);
    assert_eq!(
        forecast.surplus,
        Some(usd(999_000)),
        "bills before as_of fall outside the window"
    );
    assert_eq!(forecast.items.len(), 1, "only the bill after as_of");
}

#[test]
fn positive_liability_balance_is_a_caution() {
    let card = AccountInput {
        liquidity: LiquidityClass::ShortTermLiability,
        ..cash("Card", usd(150_000))
    };
    let id = card.id;
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(1_000_000)), card],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::LiabilityBalancePositive {
            account: id,
            balance: usd(150_000),
        }],
        "names the account"
    );
    assert_provisional(&forecast);
    assert_eq!(
        usd_totals(&forecast).cash_available,
        usd(1_150_000),
        "the sum as stored"
    );
}

#[test]
fn a_zero_or_negative_liability_is_fine() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(10)),
            AccountInput {
                liquidity: LiquidityClass::ShortTermLiability,
                ..cash("Paid off", usd(0))
            },
            AccountInput {
                liquidity: LiquidityClass::ShortTermLiability,
                ..cash("Owing", usd(-5))
            },
        ],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "nothing suspicious");
    assert_eq!(forecast.surplus, Some(usd(5)), "the card is subtracted");
}

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

fn loose(uncategorized_count: u32, possible_duplicates: u32) -> LooseEnds {
    LooseEnds {
        currency: currency("USD"),
        uncategorized_count,
        uncategorized_minor: 7_000,
        possible_duplicates,
    }
}

#[test]
fn uncategorized_spending_warns_only_with_current_month_recurring() {
    let food = CategoryId::new();
    let warned = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10_000),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![loose(3, 2)],
        ..household()
    });
    assert_eq!(
        warned.cautions,
        [
            Caution::UncategorizedSpending {
                currency: currency("USD"),
                count: 3,
                total: usd(7_000),
            },
            Caution::PossibleDuplicates { count: 2 },
        ],
        "either could yet change this month's reduction"
    );
    assert_provisional(&warned);

    for items in [
        Vec::new(),
        vec![super::budget(monthly(
            "Next month",
            usd(1),
            date(2026, 12, 20),
            food,
        ))],
        vec![super::budget(monthly(
            "Already past",
            usd(1),
            date(2026, 1, 10),
            food,
        ))],
        vec![monthly("A bill to come", usd(1), date(2026, 1, 20), food)],
        vec![one_off("One-off", usd(1), date(2026, 11, 20), food)],
        vec![inflow(monthly("Pay", usd(1), date(2026, 1, 20), food))],
        vec![super::hypothetical(
            0,
            super::budget(monthly("What if", usd(1), date(2026, 1, 20), food)),
        )],
    ] {
        let forecast = run(&ForecastInput {
            items,
            loose_ends: vec![loose(3, 2)],
            ..household()
        });
        assert_eq!(forecast.cautions, [], "the number could not change");
        assert_eq!(forecast.status, ForecastStatus::Firm, "so it is firm");
    }
}

/// Categorizing that spending could close the gap, so here it can change the number.
#[test]
fn uncategorized_spending_warns_with_an_unpaid_bill() {
    let home = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Rent", usd(10_000), date(2026, 1, 10), home)],
        loose_ends: vec![loose(3, 0)],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [
            Caution::BillNotSeen {
                category: home,
                amount: usd(10_000),
            },
            Caution::UncategorizedSpending {
                currency: currency("USD"),
                count: 3,
                total: usd(7_000),
            },
        ],
        "the gap, and what might explain it"
    );
    assert_provisional(&forecast);
}

#[test]
fn loose_ends_of_another_currency_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![LooseEnds {
            currency: currency("EUR"),
            ..loose(1, 1)
        }],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "pools are per currency");
}

#[test]
fn settled_loose_ends_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![super::budget(monthly(
            "Groceries",
            usd(10),
            date(2026, 1, 20),
            food,
        ))],
        loose_ends: vec![loose(0, 0)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "nothing is loose");
}

/// Cash 1,000,000 with a floor of 500,000. A bill of 700,000 on Nov 20 leaves
/// 300,000 at November's end; pay of 800,000 only arrives on Dec 1, and a second
/// bill of 1,000,000 on Jan 2 leaves 100,000 at January's end.
fn dipping(include_income: bool) -> ForecastInput {
    let other = CategoryId::new();
    ForecastInput {
        include_income,
        floor: Some(fixed_floor(usd(500_000))),
        items: vec![
            one_off("Bill", usd(700_000), date(2026, 11, 20), other),
            inflow(one_off("Pay", usd(800_000), date(2026, 12, 1), other)),
            one_off("Bigger", usd(1_000_000), date(2027, 1, 2), other),
        ],
        ..household()
    }
}

#[test]
fn cash_dip_warns_only_with_income() {
    let forecast = run(&dipping(true));
    assert_eq!(
        forecast.cautions,
        [Caution::CashDipsBelowFloor {
            month: "2026-11".parse::<CalendarMonth>().unwrap(),
            currency: currency("USD"),
            projected_cash: usd(300_000),
        }],
        "the first month only: income in December cannot pay November's bill"
    );
    assert_provisional(&forecast);

    let forecast = run(&dipping(false));
    assert_eq!(forecast.cautions, [], "without income, no dip is reported");
    assert_eq!(forecast.status, ForecastStatus::Firm, "and it is firm");
}

#[test]
fn cash_exactly_on_the_floor_is_no_dip() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        include_income: true,
        floor: Some(fixed_floor(usd(300_000))),
        items: vec![one_off("Bill", usd(700_000), date(2026, 11, 20), other)],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "300,000 is not below 300,000");
}

#[test]
fn a_dip_reads_each_currencys_own_floor() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        include_income: true,
        accounts: vec![cash("Checking", usd(1_000)), cash("Konto", eur(1_000))],
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 1 }),
        items: vec![
            one_off("Rechnung", eur(5_000), date(2026, 12, 1), other),
            monthly("Miete", eur(100), date(2026, 1, 20), other),
        ],
        ..household()
    });
    let dips: Vec<_> = forecast
        .cautions
        .iter()
        .filter_map(|caution| match caution {
            Caution::CashDipsBelowFloor {
                month,
                currency,
                projected_cash,
            } => Some((month.to_string(), *currency, *projected_cash)),
            _ => None,
        })
        .collect();
    assert_eq!(
        dips,
        [("2026-12".to_owned(), currency("EUR"), eur(-4_200))],
        "900 after November, then less 5,100 in December; dollars stay clear"
    );
    let totals = super::totals_of(&forecast, "EUR");
    assert_eq!(totals.floor, eur(100), "only Nov 20 is within one month");
}

#[test]
fn spent_categories_do_not_warn() {
    let food = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![monthly("Groceries", usd(10), date(2026, 1, 20), food)],
        category_spent: vec![CategorySpent {
            category: food,
            currency: currency("USD"),
            spent_minor: 1,
        }],
        ..household()
    });
    assert_eq!(forecast.cautions, [], "a reduction is not a caution");
}

fn closed_card(balance: i64) -> AccountInput {
    AccountInput {
        closed: true,
        liquidity: LiquidityClass::ShortTermLiability,
        ..cash("Old card", usd(balance))
    }
}

#[test]
fn a_closed_card_still_owing_counts_and_warns() {
    let card = closed_card(-150_000);
    let id = card.id;
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(1_000_000)), card],
        ..household()
    });
    assert_eq!(
        forecast.cautions,
        [Caution::ClosedLiabilityOwing {
            account: id,
            balance: usd(-150_000),
        }],
        "names the account and what it owes"
    );
    assert_provisional(&forecast);
    assert_eq!(
        forecast.surplus,
        Some(usd(850_000)),
        "the debt comes off cash"
    );
    let line = forecast.accounts.get(1).unwrap();
    assert!(line.counted, "it counts");
    assert_eq!(line.not_counted, None, "with no reason not to");
}

#[test]
fn a_closed_card_owing_nothing_is_left_out() {
    for balance in [0, 500] {
        let forecast = run(&ForecastInput {
            accounts: vec![cash("Checking", usd(1_000)), closed_card(balance)],
            ..household()
        });
        assert_eq!(forecast.status, ForecastStatus::Firm, "nothing owed");
        assert_eq!(forecast.surplus, Some(usd(1_000)), "not in the sum");
        assert_eq!(
            forecast.accounts.get(1).unwrap().not_counted,
            Some(crate::forecast::NotCountedReason::Closed),
            "closed says so"
        );
    }
}

#[test]
fn other_closed_debts_stay_out() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(1_000)),
            AccountInput {
                liquidity: LiquidityClass::LongTermLiability,
                ..closed_card(-500)
            },
            AccountInput {
                include_in_forecast: false,
                ..closed_card(-500)
            },
            AccountInput {
                balance: None,
                ..closed_card(0)
            },
        ],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "none of them counts");
    assert_eq!(forecast.surplus, Some(usd(1_000)), "not in the sum");
}

/// A debt is no cash to forecast from, so a closed card alone leaves no number.
#[test]
fn a_closed_card_alone_has_no_number() {
    let forecast = run(&ForecastInput {
        accounts: vec![closed_card(-500)],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [crate::forecast::Blocker::NoCountedAccounts],
        "no open account counts"
    );
    assert_eq!(forecast.surplus, None, "no number");
}
