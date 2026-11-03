//! Whether the two legs of a transfer link across currencies agree at an exchange rate
//! (spec §8.12 T8). Only a check: what moved is never changed by it, and no forecast number
//! depends on a rate because of it. Exact: amounts are compared in `i128`.

use jiff::civil::Date;
use serde::Serialize;

use crate::fx::{Conversion, RATE_STALE_AFTER_DAYS, RateBook, convert};
use crate::ids::TransferLinkId;
use crate::money::Money;

/// How far the inflow may be from the outflow converted, as a share of the converted amount.
/// A bank's spread and a fee are usually inside it; 1,250 USD against 12.50 EUR is far outside.
pub const RATE_TOLERANCE_PERCENT: i128 = 5;

/// Minor units of the inflow's currency that never count as a difference, however small the
/// amount: converting a few cents rounds by up to half a unit.
pub const ROUNDING_ALLOWANCE_MINOR: i128 = 1;

/// The legs of a link whose two currencies differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossCurrencyLink {
    pub link: TransferLinkId,
    /// Negative: what left.
    pub outflow: Money,
    /// Positive: what arrived.
    pub inflow: Money,
    pub outflow_on: Date,
    pub inflow_on: Date,
}

impl CrossCurrencyLink {
    /// The day whose rates the legs are compared at: the later of the two posted days, when
    /// the movement was complete.
    #[must_use]
    pub fn rate_day(&self) -> Date {
        self.outflow_on.max(self.inflow_on)
    }
}

/// Two legs that differ by more than the tolerance at the rate used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RateMismatch {
    /// What left, negative.
    pub outflow: Money,
    /// What arrived.
    pub inflow: Money,
    /// What the outflow comes to in the inflow's currency at the rate used, as a positive
    /// amount.
    pub expected_inflow: Money,
    /// The date of the older rate used, which a weekend or holiday carries forward.
    pub rate_on: Date,
    /// An admin entered a rate used.
    pub manual_rate: bool,
}

/// What comparing the legs at a rate came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RateCheck {
    /// The legs are within the tolerance of each other. `rate_on` is the date of the older
    /// rate used.
    Agrees { rate_on: Date },
    /// The legs differ by more than the tolerance.
    Mismatch(RateMismatch),
    /// No usable rate: none is held for one of the currencies on or before the day, the one
    /// held is more than [`RATE_STALE_AFTER_DAYS`] older than the day, the converted amount
    /// is too large, or the legs no longer are one out and one in. Nothing is said either way.
    NotChecked,
}

/// Compares the legs of `link` at the rates for the day they posted.
#[must_use]
pub fn check_rate(link: &CrossCurrencyLink, rates: &RateBook) -> RateCheck {
    // A provider's revision can leave two legs of one sign, which is no movement to compare.
    if link.outflow.minor_units >= 0 || link.inflow.minor_units <= 0 {
        return RateCheck::NotChecked;
    }
    let converted = convert(
        link.outflow.minor_units,
        link.outflow.currency,
        link.inflow.currency,
        link.rate_day(),
        rates,
    );
    let (expected, rate_on, manual_rate) = match converted {
        Conversion::Converted {
            minor,
            rate_on,
            manual,
            ..
        } => (minor, rate_on, manual.is_some()),
        // `Same` is for one currency, which a cross-currency link never has.
        Conversion::Same { .. } | Conversion::Missing { .. } | Conversion::TooLarge => {
            return RateCheck::NotChecked;
        }
    };
    // A rate carried forward over a long gap says little about the day.
    let stale = rate_on
        .until(link.rate_day())
        .is_ok_and(|span| span.get_days() > RATE_STALE_AFTER_DAYS);
    if stale {
        return RateCheck::NotChecked;
    }
    let expected = i128::from(expected).abs();
    let gap = (i128::from(link.inflow.minor_units) - expected).abs();
    if gap <= ROUNDING_ALLOWANCE_MINOR || gap * 100 <= expected * RATE_TOLERANCE_PERCENT {
        return RateCheck::Agrees { rate_on };
    }
    // A converted amount is a magnitude that fit in `i64`.
    let Ok(expected_minor) = i64::try_from(expected) else {
        return RateCheck::NotChecked;
    };
    RateCheck::Mismatch(RateMismatch {
        outflow: link.outflow,
        inflow: link.inflow,
        expected_inflow: Money {
            minor_units: expected_minor,
            currency: link.inflow.currency,
        },
        rate_on,
        manual_rate,
    })
}

/// A link that differs by more than the tolerance, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkMismatch {
    pub link: TransferLinkId,
    pub mismatch: RateMismatch,
}

/// The links of `links` whose legs differ by more than the tolerance, in their order.
#[must_use]
pub fn mismatches<'a>(
    links: impl IntoIterator<Item = &'a CrossCurrencyLink>,
    rates: &RateBook,
) -> Vec<LinkMismatch> {
    links
        .into_iter()
        .filter_map(|link| match check_rate(link, rates) {
            RateCheck::Mismatch(mismatch) => Some(LinkMismatch {
                link: link.link,
                mismatch,
            }),
            RateCheck::Agrees { .. } | RateCheck::NotChecked => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
