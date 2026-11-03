//! R4: what an account raises, which keeps the number but stops it being firm. Each asserts
//! the number is still present.

use jiff::civil::date;

use super::{AS_OF, assert_provisional, cash, days_before, household};
use super::{line, one_off, run, usd, usd_totals};
use crate::accounts::{BelongsTo, LiquidityClass};
use crate::forecast::{
    AccountInput, BalanceInput, Caution, FeedAlert, FeedProblem, FeedProblemReason, FeedStatus,
    Forecast, ForecastInput, ForecastStatus, UnboundAccount,
};
use crate::ids::{AccountId, CategoryId, ConnectionId, ProviderAccountId};

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

/// Looking ahead, a balance dated today is as fresh as a balance can be.
#[test]
fn a_future_as_of_leaves_a_balance_dated_today_fresh() {
    let forecast = run(&ForecastInput {
        as_of: date(2026, 12, 15),
        accounts: vec![aged(0)],
        ..household()
    });
    assert!(
        !forecast
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::BalanceStale { .. })),
        "only as_of is in the future"
    );
}

#[test]
fn a_future_as_of_ages_a_balance_against_today() {
    let account = aged(8);
    let id = account.id;
    let forecast = run(&ForecastInput {
        as_of: date(2026, 12, 15),
        accounts: vec![account],
        ..household()
    });
    assert!(
        forecast.cautions.contains(&Caution::BalanceStale {
            account: id,
            on: date(2026, 11, 7),
            age_days: 8,
        }),
        "eight days old today, not thirty-eight"
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

fn alert(connection: ConnectionId, serves: Vec<AccountId>, has_accounts: bool) -> FeedAlert {
    FeedAlert {
        connection,
        label: "Main bank".to_owned(),
        status: FeedStatus::ReauthRequired,
        serves,
        has_accounts,
    }
}

fn held(name: &str) -> AccountInput {
    AccountInput {
        belongs_to: BelongsTo::SomeoneElse,
        ..cash(name, usd(1))
    }
}

/// A forecast beside Checking, whose accounts are `accounts`, with one failing connection.
fn with_failing(accounts: Vec<AccountInput>, serves: &[usize]) -> (Forecast, ConnectionId) {
    failing(accounts, serves, true)
}

fn failing(
    accounts: Vec<AccountInput>,
    serves: &[usize],
    has_accounts: bool,
) -> (Forecast, ConnectionId) {
    let connection = ConnectionId::new();
    let serves = serves.iter().map(|&at| accounts[at].id).collect();
    let forecast = run(&ForecastInput {
        accounts,
        feeds: vec![alert(connection, serves, has_accounts)],
        ..household()
    });
    (forecast, connection)
}

#[test]
fn a_failing_connection_serving_a_counted_account_cautions() {
    let checking = cash("Checking", usd(1_000));
    let (forecast, connection) = with_failing(vec![checking], &[0]);
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

/// The caution alone is what makes a forecast provisional: nothing else is raised here.
#[test]
fn a_failing_connection_serving_a_counted_and_a_held_account_cautions() {
    let (forecast, _) = with_failing(vec![cash("Checking", usd(1_000)), held("Child")], &[0, 1]);
    assert_eq!(forecast.cautions.len(), 1);
    assert_provisional(&forecast);
}

#[test]
fn a_failing_connection_serving_only_a_held_account_with_no_goal_spending_changes_nothing() {
    let (forecast, _) = with_failing(vec![cash("Checking", usd(1_000)), held("Child")], &[1]);
    assert_eq!(forecast.cautions, [], "its money is not in the sum");
    assert_eq!(forecast.status, ForecastStatus::Firm);
}

#[test]
fn a_failing_connection_serving_only_an_excluded_account_changes_nothing() {
    let excluded = AccountInput {
        include_in_forecast: false,
        ..cash("Side", usd(1))
    };
    let (forecast, _) = with_failing(vec![cash("Checking", usd(1_000)), excluded], &[1]);
    assert_eq!(forecast.cautions, []);
    assert_eq!(forecast.status, ForecastStatus::Firm);
}

/// A first sync that failed leaves the household believing the bank is connected while none
/// of its money is in the forecast.
#[test]
fn a_failing_connection_with_no_provider_account_cautions() {
    let (forecast, _) = failing(vec![cash("Checking", usd(1_000))], &[], false);
    assert_eq!(forecast.cautions.len(), 1);
    assert_provisional(&forecast);
}

/// Provider accounts that are known and none of them matters: not bound to anything, and
/// nothing waiting to be bound.
#[test]
fn a_failing_connection_with_accounts_none_counted_or_unbound_changes_nothing() {
    let (forecast, _) = failing(vec![cash("Checking", usd(1_000))], &[], true);
    assert_eq!(forecast.cautions, []);
    assert_eq!(forecast.status, ForecastStatus::Firm);
}

/// Spending linked to a goal lowers its need on any account, so a stalled feed there can
/// change the number even when the account's balance is not summed.
#[test]
fn a_failing_connection_serving_an_account_with_goal_spending_cautions() {
    for account in [
        AccountInput {
            goal_spending: true,
            ..held("Child")
        },
        AccountInput {
            goal_spending: true,
            include_in_forecast: false,
            ..cash("Side", usd(1))
        },
    ] {
        let (forecast, _) = with_failing(vec![cash("Checking", usd(1_000)), account], &[1]);
        assert_eq!(forecast.cautions.len(), 1);
        assert_provisional(&forecast);
    }
}

/// An account a member may yet bind as counted is a reason to look at the connection.
#[test]
fn a_failing_connection_with_an_unbound_provider_account_cautions() {
    let connection = ConnectionId::new();
    let forecast = run(&ForecastInput {
        unbound: vec![UnboundAccount {
            provider_account: ProviderAccountId::new(),
            connection,
            name: "Savings".to_owned(),
            institution: None,
            currency_raw: "USD".to_owned(),
        }],
        feeds: vec![alert(connection, Vec::new(), true)],
        ..household()
    });
    assert!(
        forecast
            .cautions
            .iter()
            .any(|caution| matches!(caution, Caution::ConnectionNeedsAttention { .. })),
        "it may yet feed a counted account"
    );
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
        "last month's was paid, by the fixture, so only the one after as_of counts"
    );
    assert_eq!(line(&forecast, "Before").counted, usd(0), "paid");
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
