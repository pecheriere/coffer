//! R1, R4 and R5: when there is no number at all.

use jiff::civil::date;

use super::{cash, eur, every_month, fixed_floor, household, one_off, run, stored};
use super::{currency, totals_of, usd};
use crate::forecast::{AccountInput, Blocker, ForecastInput, ForecastStatus, NotCountedReason};
use crate::ids::CategoryId;
use crate::plan::SafetyFloor;

#[test]
fn mixed_currencies_block_and_report_each_currency() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(1_000_000)),
            cash("Konto", eur(500_000)),
        ],
        floor: Some(fixed_floor(usd(200_000))),
        items: vec![
            one_off("Bill", usd(100_000), date(2026, 12, 1), other),
            one_off("Rechnung", eur(50_000), date(2026, 12, 2), other),
        ],
        ..household()
    });
    assert_eq!(
        forecast.status,
        ForecastStatus::Unavailable,
        "no one number"
    );
    assert_eq!(forecast.surplus, None, "currencies are never netted");
    assert_eq!(
        forecast.blockers,
        [Blocker::MixedCurrencies {
            currencies: vec![currency("EUR"), currency("USD")],
        }],
        "the blocker names each currency"
    );

    let dollars = totals_of(&forecast, "USD");
    assert_eq!(dollars.cash_available, usd(1_000_000), "dollars held");
    assert_eq!(dollars.cash_needed, usd(300_000), "bill plus floor");
    assert_eq!(dollars.surplus, Some(usd(700_000)), "dollar surplus");

    let euros = totals_of(&forecast, "EUR");
    assert_eq!(euros.cash_available, eur(500_000), "euros held");
    assert_eq!(euros.floor, eur(0), "the fixed floor is in dollars");
    assert_eq!(euros.cash_needed, eur(50_000), "the euro bill");
    assert_eq!(euros.surplus, Some(eur(450_000)), "euro surplus");

    let lines: Vec<_> = forecast
        .months
        .first()
        .unwrap()
        .lines
        .iter()
        .map(|line| line.currency)
        .collect();
    assert_eq!(
        lines,
        [currency("EUR"), currency("USD")],
        "each month has a line per currency"
    );
}

#[test]
fn foreign_item_outside_horizon_is_not_mixed() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        items: vec![one_off("Later", eur(50_000), date(2028, 1, 1), other)],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "not in the window");
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "the number stands");
    assert_eq!(forecast.totals.len(), 1, "no row for an unused currency");
}

#[test]
fn foreign_uncounted_account_is_not_mixed() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(1_000)),
            AccountInput {
                include_in_forecast: false,
                ..cash("Konto", eur(5))
            },
        ],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "it does not count");
    assert_eq!(forecast.totals.len(), 1, "no row for it");
}

#[test]
fn foreign_fixed_floor_is_mixed() {
    let forecast = run(&ForecastInput {
        floor: Some(fixed_floor(eur(100))),
        ..household()
    });
    assert_eq!(
        forecast.surplus, None,
        "a euro floor cannot net with dollars"
    );
    assert_eq!(forecast.blockers.len(), 1, "mixed");
    assert_eq!(
        totals_of(&forecast, "EUR").surplus,
        Some(eur(-100)),
        "euros"
    );
}

/// The item falls outside the one-month window but inside the floor's twelve
/// months. Leaving that floor out of the number would hide money the household
/// must hold, and it cannot be netted with dollars, so it is a second currency.
#[test]
fn months_floor_in_another_currency_is_mixed() {
    let forecast = run(&ForecastInput {
        horizon_months: 1,
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 12 }),
        items: vec![stored("Later", eur(70_000), every_month(date(2027, 3, 1)))],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::MixedCurrencies {
            currencies: vec![currency("EUR"), currency("USD")],
        }],
        "a floor in euros is a second currency"
    );
    assert_eq!(forecast.surplus, None, "no single number");
    let euros = totals_of(&forecast, "EUR");
    assert_eq!(euros.floor, eur(630_000), "March to November 2027");
    assert_eq!(euros.surplus, Some(eur(-630_000)), "and none held");
    assert_eq!(
        forecast.items.len(),
        0,
        "the item itself is outside the window"
    );
}

#[test]
fn floor_not_set_blocks() {
    let forecast = run(&ForecastInput {
        floor: None,
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Unavailable, "no number");
    assert_eq!(forecast.surplus, None, "no silent floor of 0");
    assert_eq!(forecast.blockers, [Blocker::FloorNotSet], "the reason");
    let totals = totals_of(&forecast, "USD");
    assert_eq!(totals.surplus, None, "nor per currency");
    assert_eq!(
        totals.cash_available,
        usd(1_000_000),
        "the cash is still shown"
    );
}

#[test]
fn zero_floor_is_firm() {
    let forecast = run(&household());
    assert_eq!(
        forecast.status,
        ForecastStatus::Firm,
        "an explicit 0 is fine"
    );
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "the whole balance");
}

#[test]
fn no_counted_accounts_blocks() {
    let forecast = run(&ForecastInput {
        accounts: vec![AccountInput {
            closed: true,
            ..cash("Old", usd(10))
        }],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::NoCountedAccounts],
        "nothing counts"
    );
    assert_eq!(forecast.surplus, None, "no number");
    let account = forecast.accounts.first().unwrap();
    assert_eq!(
        account.not_counted,
        Some(NotCountedReason::Closed),
        "closed says so"
    );
}

#[test]
fn an_empty_household_has_no_number() {
    let forecast = run(&ForecastInput {
        accounts: Vec::new(),
        floor: None,
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::FloorNotSet, Blocker::NoCountedAccounts],
        "both reasons"
    );
    assert_eq!(forecast.status, ForecastStatus::Unavailable, "no number");
    assert_eq!(forecast.surplus, None, "not surplus 0");
    assert_eq!(forecast.totals, [], "no currency in use");
}

#[test]
fn counted_account_without_balance_blocks() {
    let empty = AccountInput {
        balance: None,
        ..cash("New", usd(0))
    };
    let id = empty.id;
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(100)), empty],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::BalanceMissing {
            account: id,
            name: "New".to_owned(),
        }],
        "it could hold anything"
    );
    assert_eq!(forecast.surplus, None, "no number");
    assert_eq!(
        totals_of(&forecast, "USD").surplus,
        None,
        "nor per currency"
    );
    let line = forecast.accounts.get(1).unwrap();
    assert!(line.counted, "the account does count");
    assert_eq!(line.balance, None, "no balance to show");
    assert_eq!(line.balance_on, None, "no date to show");
}

#[test]
fn excluded_account_without_balance_does_not_block() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(100)),
            AccountInput {
                balance: None,
                include_in_forecast: false,
                ..cash("Side", usd(0))
            },
        ],
        ..household()
    });
    assert_eq!(forecast.status, ForecastStatus::Firm, "it does not count");
    assert_eq!(forecast.surplus, Some(usd(100)), "the number stands");
}

#[test]
fn reasons_prefer_the_most_final() {
    let forecast = run(&ForecastInput {
        accounts: vec![
            cash("Checking", usd(100)),
            AccountInput {
                closed: true,
                include_in_forecast: false,
                liquidity: crate::accounts::LiquidityClass::Illiquid,
                ..cash("Gone", usd(5))
            },
            AccountInput {
                include_in_forecast: false,
                liquidity: crate::accounts::LiquidityClass::LongTermLiability,
                ..cash("Mortgage", usd(-5))
            },
        ],
        ..household()
    });
    let reasons: Vec<_> = forecast.accounts.iter().map(|a| a.not_counted).collect();
    assert_eq!(
        reasons,
        [
            None,
            Some(NotCountedReason::Closed),
            Some(NotCountedReason::NotCash),
        ],
        "closed, then not cash, then the flag"
    );
    assert_eq!(forecast.surplus, Some(usd(100)), "neither joins the sum");
}
