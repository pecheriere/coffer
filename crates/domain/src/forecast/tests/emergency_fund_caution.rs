//! §10.5: a currency in use with no emergency fund target holds nothing back, with a caution,
//! where it used to leave the forecast without a number.

use jiff::civil::date;

use super::{cash, currency, eur, fixed_fund, household, one_off, run, totals_of, usd};
use crate::forecast::{Blocker, Caution, ForecastInput, ForecastStatus};
use crate::ids::CategoryId;
use crate::plan::EmergencyFunds;

#[test]
fn no_target_is_a_caution_not_a_blocker() {
    let forecast = run(&ForecastInput {
        emergency_funds: EmergencyFunds::new(),
        ..household()
    });
    assert_eq!(forecast.blockers, [], "a number all the same");
    assert_eq!(
        forecast.cautions,
        [Caution::EmergencyFundNotSet {
            currency: currency("USD"),
        }],
        "naming the currency"
    );
    assert_eq!(forecast.status, ForecastStatus::Provisional, "not firm");
    let totals = totals_of(&forecast, "USD");
    assert_eq!(totals.emergency_fund, usd(0), "nothing held back");
    assert_eq!(forecast.surplus, Some(usd(1_000_000)), "the whole balance");
    assert_eq!(totals.surplus, Some(usd(1_000_000)), "per currency too");
    assert_eq!(forecast.emergency_funds, [], "no target to list");
}

/// With dollars targeted and euros not, euros hold nothing back with a caution, and dollars'
/// target is never applied to euros. Mixed currencies still leave no single number.
#[test]
fn a_target_applies_only_to_its_currency() {
    let other = CategoryId::new();
    let forecast = run(&ForecastInput {
        accounts: vec![cash("Checking", usd(500_000)), cash("Konto", eur(10_000))],
        emergency_funds: fixed_fund(usd(100_000)),
        items: vec![one_off("Bill", usd(20_000), date(2026, 12, 1), other)],
        ..household()
    });
    assert_eq!(
        forecast.blockers,
        [Blocker::MixedCurrencies {
            currencies: vec![currency("EUR"), currency("USD")],
        }],
        "only the mix blocks"
    );
    assert!(
        forecast.cautions.contains(&Caution::EmergencyFundNotSet {
            currency: currency("EUR"),
        }),
        "euros have no target"
    );
    assert!(
        !forecast.cautions.contains(&Caution::EmergencyFundNotSet {
            currency: currency("USD"),
        }),
        "dollars do"
    );
    let dollars = totals_of(&forecast, "USD");
    assert_eq!(dollars.emergency_fund, usd(100_000), "dollars' own target");
    assert_eq!(
        dollars.cash_needed,
        usd(120_000),
        "bill plus emergency fund"
    );
    assert_eq!(dollars.surplus, Some(usd(380_000)), "dollars' own number");
    let euros = totals_of(&forecast, "EUR");
    assert_eq!(euros.emergency_fund, eur(0), "nothing from dollars' target");
    assert_eq!(
        euros.surplus,
        Some(eur(10_000)),
        "all of euros' cash is left"
    );
}
