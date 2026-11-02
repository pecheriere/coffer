//! R4: what an account raises, which keeps the number but stops it being firm. Each asserts
//! the number is still present.

use jiff::civil::date;

use super::{AS_OF, assert_provisional, cash, days_before, household};
use super::{one_off, run, usd, usd_totals};
use crate::accounts::LiquidityClass;
use crate::forecast::{
    AccountInput, BalanceInput, Caution, FeedAlert, FeedProblem, FeedProblemReason, FeedStatus,
    ForecastInput, ForecastStatus, UnboundAccount,
};
use crate::ids::{CategoryId, ConnectionId, ProviderAccountId};

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
