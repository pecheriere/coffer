//! Exchange rates and conversion (spec §8.10 N4, N5). Rates are units of a
//! currency per one euro, held exactly; a conversion goes through the euro in
//! checked `i128` and rounds half-even once. No floating point.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::money::Currency;
use crate::transactions::string_enum;

/// A rate more than this many days older than the day it values raises a
/// caution: a discontinued currency would otherwise convert at its last rate
/// for ever.
pub const RATE_STALE_AFTER_DAYS: i32 = 7;

/// How many days either side of its own a manual rate is compared with a published one.
pub const UNUSUAL_RATE_NEARBY_DAYS: i64 = 30;

/// The most digits a rate holds, on both sides of the point together.
const MAX_RATE_DIGITS: usize = 10;
/// The most decimals a rate holds.
const MAX_RATE_DECIMALS: usize = 6;
/// A rate's integer value per unit: millionths, its finest decimal.
const RATE_SCALE: u64 = 1_000_000;

/// Why an exchange rate value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FxError {
    #[error("malformed rate: expected at most 10 digits, 6 of them decimals, above zero")]
    MalformedRate,
    #[error("unknown rate source")]
    UnknownSource,
    #[error("unknown rate fetch status")]
    UnknownFetchStatus,
    #[error("unknown rate error code")]
    UnknownErrorCode,
    #[error("unknown rate leg")]
    UnknownRateLeg,
}

/// Units of a currency per one euro, exactly. Text outside the program:
/// `1.1150` and `1.115` are the same rate, which prints `1.115`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FxRate {
    /// In millionths. At most 10 digits with at most 6 decimals, so below
    /// `10^16`, which leaves a conversion inside `i128`.
    millionths: u64,
}

impl FxRate {
    /// One euro per euro.
    const ONE: Self = Self {
        millionths: RATE_SCALE,
    };
}

impl FromStr for FxRate {
    type Err = FxError;

    /// Accepts `^[0-9]{1,10}(\.[0-9]{1,6})?$` with at most 10 digits in all,
    /// above zero. No exponent, sign or space.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (int, decimals) = input.split_once('.').unwrap_or((input, "0"));
        let digits = |part: &str, most| {
            (1..=most).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
        };
        let written = int.len()
            + if input.contains('.') {
                decimals.len()
            } else {
                0
            };
        if !digits(int, MAX_RATE_DIGITS)
            || !digits(decimals, MAX_RATE_DECIMALS)
            || written > MAX_RATE_DIGITS
        {
            return Err(FxError::MalformedRate);
        }
        // At most 10 digits, then padded to 6 decimals: within `u64`.
        let padding = MAX_RATE_DECIMALS - decimals.len();
        let millionths = int
            .bytes()
            .chain(decimals.bytes())
            .chain(std::iter::repeat_n(b'0', padding))
            .fold(0_u64, |acc, digit| acc * 10 + u64::from(digit - b'0'));
        if millionths == 0 {
            return Err(FxError::MalformedRate);
        }
        Ok(Self { millionths })
    }
}

impl fmt::Display for FxRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (int, decimals) = (self.millionths / RATE_SCALE, self.millionths % RATE_SCALE);
        if decimals == 0 {
            return write!(f, "{int}");
        }
        let padded = format!("{decimals:0MAX_RATE_DECIMALS$}");
        write!(f, "{int}.{}", padded.trim_end_matches('0'))
    }
}

impl From<FxRate> for String {
    fn from(rate: FxRate) -> Self {
        rate.to_string()
    }
}

impl TryFrom<String> for FxRate {
    type Error = FxError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for FxRate {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some(r"^[0-9]{1,10}(\.[0-9]{1,6})?$"))
            .description(Some(
                "Units of a currency per one euro, exact, as text: at most 10 digits, \
                 6 of them decimals, without trailing zeros. Never a JSON number.",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for FxRate {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("FxRate")
    }
}

string_enum!(
    /// Who published a rate.
    FxSource, FxError::UnknownSource,
    {
        /// The European Central Bank's reference rates.
        Ecb => "ecb",
    }
);

string_enum!(
    /// How fetching a currency's rates last went.
    FxFetchStatus, FxError::UnknownFetchStatus,
    {
        /// Rates arrived.
        Ready => "ready",
        /// The provider has no such series.
        NotPublished => "not_published",
        /// The provider could not be read; it will be tried again.
        Failing => "failing",
    }
);

string_enum!(
    /// Why a fetch failed. A fixed code, never the provider's own text.
    FxErrorCode, FxError::UnknownErrorCode,
    {
        /// The provider answered with an error.
        Unavailable => "unavailable",
        /// The provider's answer could not be read.
        Malformed => "malformed",
        /// The provider could not be reached.
        Network => "network",
    }
);

string_enum!(
    /// Which side of a conversion a rate belongs to: what a rate date or a missing
    /// rate is about. The euro is 1 on every day, so it is never the one.
    RateLeg, FxError::UnknownRateLeg,
    {
        /// The amount's own currency.
        Held => "held",
        /// The currency it is shown in.
        Display => "display",
        /// Both: the same rate date on each side, or no rate on either.
        Both => "both",
    }
);

/// One day's rate of one currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RateObservation {
    pub on: Date,
    pub rate: FxRate,
}

/// One day's rate of one currency as a calculation uses it: on a day with both, an
/// admin's rate rather than the published one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HeldRate {
    pub on: Date,
    pub rate: FxRate,
    /// An admin entered this rate; no source published it.
    pub manual: bool,
}

/// A rate an admin entered for one currency and one day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManualRate {
    pub currency: Currency,
    pub on: Date,
    pub rate: FxRate,
    /// When it was last entered. Who entered it is in the audit log.
    pub entered_at: Timestamp,
}

/// Which days a currency has rates an admin entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManualCoverage {
    pub currency: Currency,
    /// The first day with a manual rate.
    pub first_on: Date,
    /// The last day with a manual rate.
    pub last_on: Date,
}

/// What is known about fetching one currency's rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FxCoverage {
    pub currency: Currency,
    pub status: FxFetchStatus,
    /// The first day with a rate.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub first_on: Option<Date>,
    /// The last day with a rate.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub last_on: Option<Date>,
    pub last_attempt_at: Timestamp,
    #[cfg_attr(feature = "openapi", schema(required))]
    pub last_success_at: Option<Timestamp>,
    /// Why the last attempt failed; `null` when it did not.
    #[cfg_attr(feature = "openapi", schema(required))]
    pub last_error: Option<FxErrorCode>,
}

/// One currency's rates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateHistory {
    /// Sorted by date, one per day.
    rates: Vec<HeldRate>,
}

impl RateHistory {
    /// A history of published `rates`, in any order.
    #[must_use]
    pub fn new(rates: Vec<RateObservation>) -> Self {
        Self::merged(rates, Vec::new())
    }

    /// A history of `published` and `manual` rates, each in any order. The one rule
    /// joining them: a manual rate wins over a published rate on its own day, and a
    /// published rate on a later day takes over from it as any later rate would.
    #[must_use]
    pub fn merged(published: Vec<RateObservation>, manual: Vec<RateObservation>) -> Self {
        let held = |manual| {
            move |observation: RateObservation| {
                let RateObservation { on, rate } = observation;
                (on, HeldRate { on, rate, manual })
            }
        };
        let by_day: BTreeMap<Date, HeldRate> = published
            .into_iter()
            .map(held(false))
            .chain(manual.into_iter().map(held(true)))
            .collect();
        Self {
            rates: by_day.into_values().collect(),
        }
    }

    /// Every rate, oldest first.
    #[must_use]
    pub fn rates(&self) -> &[HeldRate] {
        &self.rates
    }

    /// The most recent rate on or before `on`, never a later one.
    #[must_use]
    pub fn on_or_before(&self, on: Date) -> Option<HeldRate> {
        let after = self.rates.partition_point(|held| held.on <= on);
        self.rates.get(after.checked_sub(1)?).copied()
    }
}

/// The rates a calculation may use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateBook {
    histories: BTreeMap<Currency, RateHistory>,
}

/// What a [`RateBook`] holds for a currency on a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLookup {
    /// The euro is 1 on every day, published by nobody.
    Euro,
    /// The most recent rate on or before the day.
    Found(HeldRate),
    Missing,
}

impl RateBook {
    /// Sets `currency`'s rates, replacing any it had.
    pub fn insert(&mut self, currency: Currency, history: RateHistory) {
        self.histories.insert(currency, history);
    }

    /// `currency`'s rate for `on`.
    #[must_use]
    pub fn rate(&self, currency: Currency, on: Date) -> RateLookup {
        if is_euro(currency) {
            return RateLookup::Euro;
        }
        self.histories
            .get(&currency)
            .and_then(|history| history.on_or_before(on))
            .map_or(RateLookup::Missing, RateLookup::Found)
    }
}

/// The published rate nearest `on` among `nearby`, if `entered` is more than 1.5 times away
/// from it in either direction, the earlier of two as near. No published rate says nothing.
/// A rate entered the wrong way round is caught only when the true one is above about 1.22 or
/// below about 0.82: closer to 1, its inverse is within 1.5 times.
#[must_use]
pub fn unusual_against(
    entered: FxRate,
    on: Date,
    nearby: &[RateObservation],
) -> Option<RateObservation> {
    let nearest = nearby
        .iter()
        .filter_map(|one| {
            let distance = on.until(one.on).ok()?.get_days().abs();
            Some((distance, one.on, *one))
        })
        .min_by_key(|&(distance, day, _)| (distance, day))
        .map(|(_, _, one)| one)?;
    let (ours, theirs) = (
        u128::from(entered.millionths),
        u128::from(nearest.rate.millionths),
    );
    (ours * 2 > theirs * 3 || theirs * 2 > ours * 3).then_some(nearest)
}

/// Whether `currency` is the euro, which every rate is quoted against.
pub(crate) fn is_euro(currency: Currency) -> bool {
    currency.as_str() == "EUR"
}

/// What converting an amount came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// The currencies are the same, so no rate was involved.
    Same { minor: i64 },
    /// `rate_on` is the older of the rate dates used, and `leg` the side it is on.
    /// `manual` is the side using a rate an admin entered, if any does.
    Converted {
        minor: i64,
        rate_on: Date,
        leg: RateLeg,
        manual: Option<RateLeg>,
    },
    /// A rate on or before the day is missing, on `leg`.
    Missing { leg: RateLeg },
    /// The magnitude of the result does not fit in `i64`.
    TooLarge,
}

/// Converts `minor` units of `from` into `to`, at the rates for `on`:
/// `minor × to_rate / from_rate × 10^(to.exponent − from.exponent)`, half-even
/// and symmetric for negatives.
#[must_use]
pub fn convert(minor: i64, from: Currency, to: Currency, on: Date, book: &RateBook) -> Conversion {
    if from == to {
        return Conversion::Same { minor };
    }
    let (from_rate, to_rate, rate_on, leg, manual) = match (book.rate(from, on), book.rate(to, on))
    {
        (RateLookup::Missing, RateLookup::Missing) => {
            return Conversion::Missing { leg: RateLeg::Both };
        }
        (RateLookup::Missing, _) => return Conversion::Missing { leg: RateLeg::Held },
        (_, RateLookup::Missing) => {
            return Conversion::Missing {
                leg: RateLeg::Display,
            };
        }
        (RateLookup::Euro, RateLookup::Euro) => return Conversion::Same { minor },
        (RateLookup::Euro, RateLookup::Found(to)) => (
            FxRate::ONE,
            to.rate,
            to.on,
            RateLeg::Display,
            manual_leg(false, to.manual),
        ),
        (RateLookup::Found(from), RateLookup::Euro) => (
            from.rate,
            FxRate::ONE,
            from.on,
            RateLeg::Held,
            manual_leg(from.manual, false),
        ),
        (RateLookup::Found(from), RateLookup::Found(to)) => {
            let leg = match from.on.cmp(&to.on) {
                Ordering::Less => RateLeg::Held,
                Ordering::Greater => RateLeg::Display,
                Ordering::Equal => RateLeg::Both,
            };
            (
                from.rate,
                to.rate,
                from.on.min(to.on),
                leg,
                manual_leg(from.manual, to.manual),
            )
        }
    };
    let magnitude = scaled_magnitude(
        minor.unsigned_abs(),
        from_rate,
        to_rate,
        from.exponent(),
        to.exponent(),
    );
    match magnitude.and_then(|magnitude| i64::try_from(magnitude).ok()) {
        Some(magnitude) if minor < 0 => Conversion::Converted {
            minor: -magnitude,
            rate_on,
            leg,
            manual,
        },
        Some(magnitude) => Conversion::Converted {
            minor: magnitude,
            rate_on,
            leg,
            manual,
        },
        None => Conversion::TooLarge,
    }
}

/// The side of a conversion using a manual rate, if any does.
fn manual_leg(held: bool, display: bool) -> Option<RateLeg> {
    match (held, display) {
        (false, false) => None,
        (true, false) => Some(RateLeg::Held),
        (false, true) => Some(RateLeg::Display),
        (true, true) => Some(RateLeg::Both),
    }
}

/// `magnitude × to_rate / from_rate × 10^(to_exponent − from_exponent)`, rounded
/// half-even. At most `2^63 × 10^16 × 10^3`, about `9.3 × 10^37`, which `i128`
/// holds; checked all the same.
fn scaled_magnitude(
    magnitude: u64,
    from_rate: FxRate,
    to_rate: FxRate,
    from_exponent: u8,
    to_exponent: u8,
) -> Option<i128> {
    let ten_to = |power: u8| 10_i128.checked_pow(u32::from(power));
    let numerator = i128::from(magnitude)
        .checked_mul(i128::from(to_rate.millionths))?
        .checked_mul(ten_to(to_exponent.saturating_sub(from_exponent))?)?;
    let denominator = i128::from(from_rate.millionths)
        .checked_mul(ten_to(from_exponent.saturating_sub(to_exponent))?)?;
    let quotient = numerator.checked_div(denominator)?;
    let twice_remainder = numerator.checked_rem(denominator)?.checked_mul(2)?;
    let round_up = match twice_remainder.cmp(&denominator) {
        Ordering::Greater => true,
        Ordering::Equal => quotient % 2 == 1,
        Ordering::Less => false,
    };
    quotient.checked_add(i128::from(round_up))
}

#[cfg(test)]
mod tests;
