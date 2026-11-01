//! Money: ISO 4217 currencies, integer minor units and exact decimal parsing.
//!
//! Every amount is an integer number of minor units plus its currency; no
//! floating point is involved anywhere. Negative means money out of the
//! account.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Why a money value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("unknown currency: expected an active ISO 4217 code")]
    UnknownCurrency,
    #[error("malformed amount: expected digits with an optional '-' and decimal point")]
    Malformed,
    #[error("amount has more decimal places than its currency allows")]
    TooPrecise,
    #[error("amount is too large")]
    Overflow,
}

/// The active ISO 4217 currencies and their minor-unit exponents, sorted by
/// code.
///
/// Source: ISO 4217 List One, as published by the maintenance agency (SIX)
/// at
/// <https://www.six-group.com/dam/download/financial-information/data-center/iso-currrency/lists/list-one.xml>,
/// edition published 2026-09-17 (`Pblshd`), downloaded 2026-10-31.
///
/// An entry `<CcyNtry>` is kept when it has a `<Ccy>` code, its `<CcyNm>` is
/// not marked `IsFund="true"`, and its `<CcyMnrUnts>` is a number rather
/// than `N.A.`; a code is listed once. That rule drops the fund codes
/// (BOV, CHE, CHW, CLF, COU, MXV, USN, UYI, UYW, XAD; UYW, the Uruguayan
/// wage-index unit, is marked `IsFund="true"` in this edition, so it falls
/// under the same rule) and the codes without minor units (XAG, XAU, XPD,
/// XPT, XBA to XBD, XDR, XSU, XUA, XTS, XXX). ANG, replaced by XCG, and BGN,
/// replaced by the euro on 2026-01-01, are absent from this edition, so they
/// do not parse.
///
/// To regenerate: download the file above, apply the rule to every
/// `<CcyNtry>`, sort the codes and replace the entries below; then update
/// the edition and download dates here. `currency_table_is_sorted_and_every_code_parses`
/// checks the result.
const CURRENCIES: [(&str, u8); 155] = [
    ("AED", 2),
    ("AFN", 2),
    ("ALL", 2),
    ("AMD", 2),
    ("AOA", 2),
    ("ARS", 2),
    ("AUD", 2),
    ("AWG", 2),
    ("AZN", 2),
    ("BAM", 2),
    ("BBD", 2),
    ("BDT", 2),
    ("BHD", 3),
    ("BIF", 0),
    ("BMD", 2),
    ("BND", 2),
    ("BOB", 2),
    ("BRL", 2),
    ("BSD", 2),
    ("BTN", 2),
    ("BWP", 2),
    ("BYN", 2),
    ("BZD", 2),
    ("CAD", 2),
    ("CDF", 2),
    ("CHF", 2),
    ("CLP", 0),
    ("CNY", 2),
    ("COP", 2),
    ("CRC", 2),
    ("CUP", 2),
    ("CVE", 2),
    ("CZK", 2),
    ("DJF", 0),
    ("DKK", 2),
    ("DOP", 2),
    ("DZD", 2),
    ("EGP", 2),
    ("ERN", 2),
    ("ETB", 2),
    ("EUR", 2),
    ("FJD", 2),
    ("FKP", 2),
    ("GBP", 2),
    ("GEL", 2),
    ("GHS", 2),
    ("GIP", 2),
    ("GMD", 2),
    ("GNF", 0),
    ("GTQ", 2),
    ("GYD", 2),
    ("HKD", 2),
    ("HNL", 2),
    ("HTG", 2),
    ("HUF", 2),
    ("IDR", 2),
    ("ILS", 2),
    ("INR", 2),
    ("IQD", 3),
    ("IRR", 2),
    ("ISK", 0),
    ("JMD", 2),
    ("JOD", 3),
    ("JPY", 0),
    ("KES", 2),
    ("KGS", 2),
    ("KHR", 2),
    ("KMF", 0),
    ("KPW", 2),
    ("KRW", 0),
    ("KWD", 3),
    ("KYD", 2),
    ("KZT", 2),
    ("LAK", 2),
    ("LBP", 2),
    ("LKR", 2),
    ("LRD", 2),
    ("LSL", 2),
    ("LYD", 3),
    ("MAD", 2),
    ("MDL", 2),
    ("MGA", 2),
    ("MKD", 2),
    ("MMK", 2),
    ("MNT", 2),
    ("MOP", 2),
    ("MRU", 2),
    ("MUR", 2),
    ("MVR", 2),
    ("MWK", 2),
    ("MXN", 2),
    ("MYR", 2),
    ("MZN", 2),
    ("NAD", 2),
    ("NGN", 2),
    ("NIO", 2),
    ("NOK", 2),
    ("NPR", 2),
    ("NZD", 2),
    ("OMR", 3),
    ("PAB", 2),
    ("PEN", 2),
    ("PGK", 2),
    ("PHP", 2),
    ("PKR", 2),
    ("PLN", 2),
    ("PYG", 0),
    ("QAR", 2),
    ("RON", 2),
    ("RSD", 2),
    ("RUB", 2),
    ("RWF", 0),
    ("SAR", 2),
    ("SBD", 2),
    ("SCR", 2),
    ("SDG", 2),
    ("SEK", 2),
    ("SGD", 2),
    ("SHP", 2),
    ("SLE", 2),
    ("SOS", 2),
    ("SRD", 2),
    ("SSP", 2),
    ("STN", 2),
    ("SVC", 2),
    ("SYP", 2),
    ("SZL", 2),
    ("THB", 2),
    ("TJS", 2),
    ("TMT", 2),
    ("TND", 3),
    ("TOP", 2),
    ("TRY", 2),
    ("TTD", 2),
    ("TWD", 2),
    ("TZS", 2),
    ("UAH", 2),
    ("UGX", 0),
    ("USD", 2),
    ("UYU", 2),
    ("UZS", 2),
    ("VED", 2),
    ("VES", 2),
    ("VND", 0),
    ("VUV", 0),
    ("WST", 2),
    ("XAF", 0),
    ("XCD", 2),
    ("XCG", 2),
    ("XOF", 0),
    ("XPF", 0),
    ("YER", 2),
    ("ZAR", 2),
    ("ZMW", 2),
    ("ZWG", 2),
];

/// An active ISO 4217 currency, such as `USD`. Only built from an entry of
/// the built-in table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency {
    code: &'static str,
    exponent: u8,
}

impl Currency {
    /// Accepts an uppercase code from the built-in ISO 4217 table.
    ///
    /// # Errors
    /// Returns [`MoneyError::UnknownCurrency`] for anything else, including
    /// lowercase codes.
    pub fn parse(input: &str) -> Result<Self, MoneyError> {
        CURRENCIES
            .binary_search_by_key(&input, |&(code, _)| code)
            .ok()
            .and_then(|index| CURRENCIES.get(index))
            .map(|&(code, exponent)| Self { code, exponent })
            .ok_or(MoneyError::UnknownCurrency)
    }

    /// The number of decimal places of the minor unit (USD 2, JPY 0).
    #[must_use]
    pub fn exponent(self) -> u8 {
        self.exponent
    }

    /// The three-letter code.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code)
    }
}

impl Serialize for Currency {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code)
    }
}

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Self::parse(&code).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for Currency {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some("^[A-Z]{3}$"))
            .description(Some(
                "An active ISO 4217 currency code; its exponent gives the minor unit (USD 2, JPY 0).",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for Currency {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Currency")
    }
}

/// An amount: an integer number of minor units plus its ISO 4217 currency
/// code. `{"minor_units": -1234, "currency": "USD"}` is -12.34 USD; negative
/// is money out of the account. Never a floating-point number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Money {
    /// The amount in the currency's minor units: `-1234` USD is -12.34 USD.
    pub minor_units: i64,
    pub currency: Currency,
}

impl Money {
    /// The sum of two amounts of the same currency.
    ///
    /// Returns `None` when the currencies differ or the sum overflows.
    #[must_use]
    pub fn checked_add(self, other: Money) -> Option<Money> {
        if self.currency != other.currency {
            return None;
        }
        Some(Money {
            minor_units: self.minor_units.checked_add(other.minor_units)?,
            currency: self.currency,
        })
    }
}

/// The most digits on either side of the decimal point.
const MAX_DIGITS: usize = 18;

/// A decimal amount as text, as providers report it: `-?[0-9]{1,18}` with an
/// optional `.` and 1 to 18 fractional digits. It has no currency until
/// [`DecimalAmount::to_minor_units`] gives it one.
///
/// It deliberately has no equality: `"12.30"` and `"12.3"` are the same
/// amount, so compare [`DecimalAmount::to_minor_units`] results instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DecimalAmount(String);

impl DecimalAmount {
    /// Accepts `-?[0-9]{1,18}(\.[0-9]{1,18})?` only: no exponent, no `+`, no
    /// spaces, no other digits than ASCII.
    ///
    /// # Errors
    /// Returns [`MoneyError::Malformed`] for anything else.
    pub fn parse(input: &str) -> Result<Self, MoneyError> {
        let unsigned = input.strip_prefix('-').unwrap_or(input);
        let (int, frac) = match unsigned.split_once('.') {
            Some((int, frac)) => (int, Some(frac)),
            None => (unsigned, None),
        };
        let digits = |part: &str| {
            (1..=MAX_DIGITS).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
        };
        if digits(int) && frac.is_none_or(digits) {
            Ok(Self(input.to_owned()))
        } else {
            Err(MoneyError::Malformed)
        }
    }

    /// The same amount with the opposite sign. Zero has no sign: it stays as
    /// it is written.
    #[must_use]
    pub fn negated(&self) -> DecimalAmount {
        if self.0.bytes().all(|b| matches!(b, b'0' | b'.')) {
            return self.clone();
        }
        match self.0.strip_prefix('-') {
            Some(unsigned) => Self(unsigned.to_owned()),
            None => Self(format!("-{}", self.0)),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The exact amount in `currency`'s minor units.
    ///
    /// # Errors
    /// Returns [`MoneyError::TooPrecise`] when a non-zero digit lies beyond
    /// the currency's exponent, and [`MoneyError::Overflow`] when the result
    /// does not fit an `i64`.
    pub fn to_minor_units(&self, currency: Currency) -> Result<i64, MoneyError> {
        let (negative, magnitude, dropped) = self.split_at_minor_unit(currency);
        if dropped.bytes().any(|b| b != b'0') {
            return Err(MoneyError::TooPrecise);
        }
        signed_minor_units(negative, magnitude)
    }

    /// The amount in `currency`'s minor units, rounded half-even when it has
    /// more decimals. For a valuation such as a balance; a ledger amount is
    /// exact ([`Self::to_minor_units`]).
    ///
    /// # Errors
    /// Returns [`MoneyError::Overflow`] when the result does not fit an `i64`.
    pub fn to_minor_units_rounded(&self, currency: Currency) -> Result<i64, MoneyError> {
        let (negative, magnitude, dropped) = self.split_at_minor_unit(currency);
        let mut digits = dropped.bytes();
        let first = digits.next().unwrap_or(b'0');
        let beyond_half = digits.any(|b| b != b'0');
        let round_up = first > b'5' || (first == b'5' && (beyond_half || magnitude % 2 == 1));
        signed_minor_units(negative, magnitude + i128::from(round_up))
    }

    /// Whether the amount is negative, its magnitude in `currency`'s minor
    /// units, and the decimals beyond them.
    fn split_at_minor_unit(&self, currency: Currency) -> (bool, i128, &str) {
        let (negative, unsigned) = match self.0.strip_prefix('-') {
            Some(unsigned) => (true, unsigned),
            None => (false, self.0.as_str()),
        };
        let (int, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let exponent = usize::from(currency.exponent());
        let (kept, dropped) = frac.split_at(frac.len().min(exponent));
        // At most 18 + 3 digits: an i128 cannot overflow.
        let padding = exponent - kept.len();
        let magnitude = int
            .bytes()
            .chain(kept.bytes())
            .chain(std::iter::repeat_n(b'0', padding))
            .fold(0_i128, |acc, digit| acc * 10 + i128::from(digit - b'0'));
        (negative, magnitude, dropped)
    }
}

fn signed_minor_units(negative: bool, magnitude: i128) -> Result<i64, MoneyError> {
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).map_err(|_| MoneyError::Overflow)
}

impl From<DecimalAmount> for String {
    fn from(amount: DecimalAmount) -> Self {
        amount.0
    }
}

impl TryFrom<String> for DecimalAmount {
    type Error = MoneyError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for DecimalAmount {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .pattern(Some(r"^-?[0-9]{1,18}(\.[0-9]{1,18})?$"))
            .description(Some(
                "A decimal amount in major units, such as \"-12.34\"; never a JSON number.",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for DecimalAmount {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DecimalAmount")
    }
}

/// `minor` units of `currency` as a decimal in major units: `-1234` USD is
/// `"-12.34"`, `-7` JPY is `"-7"`.
#[must_use]
pub fn format_minor_units(minor: i64, currency: Currency) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let magnitude = minor.unsigned_abs();
    let exponent = currency.exponent();
    if exponent == 0 {
        return format!("{sign}{magnitude}");
    }
    let scale = 10_u64.pow(u32::from(exponent));
    let width = usize::from(exponent);
    format!("{sign}{}.{:0width$}", magnitude / scale, magnitude % scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd() -> Currency {
        Currency::parse("USD").unwrap()
    }

    #[test]
    fn currency_exponents_follow_iso4217() {
        for (code, exponent) in [("USD", 2), ("JPY", 0), ("KWD", 3), ("EUR", 2)] {
            let currency = Currency::parse(code).unwrap();
            assert_eq!(currency.exponent(), exponent, "{code}");
            assert_eq!(currency.as_str(), code);
        }
    }

    /// `parse` binary-searches the table, which needs it strictly sorted.
    #[test]
    fn currency_table_is_sorted_and_every_code_parses() {
        assert!(CURRENCIES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (code, exponent) in CURRENCIES {
            let currency = Currency::parse(code).unwrap();
            assert_eq!((currency.as_str(), currency.exponent()), (code, exponent));
            assert!(code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()));
            assert!([0, 2, 3].contains(&exponent), "{code}");
        }
    }

    #[test]
    fn currency_rejects_lowercase_unknown_and_urls() {
        for bad in [
            "usd",
            "XYZ",
            "https://x",
            "",
            "US",
            "USDX",
            "XAU",
            "XTS",
            "CHE",
        ] {
            assert_eq!(
                Currency::parse(bad),
                Err(MoneyError::UnknownCurrency),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn currency_serde_is_the_code() {
        assert_eq!(serde_json::to_string(&usd()).unwrap(), "\"USD\"");
        assert_eq!(serde_json::from_str::<Currency>("\"USD\"").unwrap(), usd());
        assert!(serde_json::from_str::<Currency>("\"usd\"").is_err());
    }

    #[test]
    fn money_serde_is_minor_units_and_code() {
        let money = Money {
            minor_units: -1234,
            currency: usd(),
        };
        assert_eq!(
            serde_json::to_string(&money).unwrap(),
            r#"{"minor_units":-1234,"currency":"USD"}"#
        );
        assert_eq!(
            serde_json::from_str::<Money>(r#"{"minor_units":-1234,"currency":"USD"}"#).unwrap(),
            money
        );
    }

    #[test]
    fn money_adds_only_same_currency_without_overflow() {
        let a = Money {
            minor_units: 100,
            currency: usd(),
        };
        let b = Money {
            minor_units: -250,
            currency: usd(),
        };
        assert_eq!(
            a.checked_add(b),
            Some(Money {
                minor_units: -150,
                currency: usd()
            })
        );
        let yen = Money {
            minor_units: 1,
            currency: Currency::parse("JPY").unwrap(),
        };
        assert_eq!(a.checked_add(yen), None);
        let max = Money {
            minor_units: i64::MAX,
            currency: usd(),
        };
        assert_eq!(max.checked_add(a), None);
    }

    #[test]
    fn decimal_to_minor_units_is_exact() {
        let minor =
            |s: &str, c: &str| DecimalAmount::parse(s)?.to_minor_units(Currency::parse(c).unwrap());
        assert_eq!(minor("-12.34", "USD"), Ok(-1234));
        assert_eq!(minor("12.340", "USD"), Ok(1234));
        assert_eq!(minor("12.345", "USD"), Err(MoneyError::TooPrecise));
        for malformed in [
            "1e3", "+1", " 1", "1 ", "", "-", ".5", "1.", "1,5", "--1", "١",
        ] {
            assert_eq!(
                DecimalAmount::parse(malformed).err(),
                Some(MoneyError::Malformed),
                "{malformed:?}"
            );
        }
        assert_eq!(
            DecimalAmount::parse(&"1".repeat(19)).err(),
            Some(MoneyError::Malformed)
        );
        assert_eq!(minor("100", "JPY"), Ok(100));
        assert_eq!(minor("100.0", "JPY"), Ok(100));
        assert_eq!(minor("100.5", "JPY"), Err(MoneyError::TooPrecise));
        assert_eq!(minor("1.234", "KWD"), Ok(1234));
        assert_eq!(
            minor("92233720368547758.08", "USD"),
            Err(MoneyError::Overflow)
        );
        assert_eq!(minor("92233720368547758.07", "USD"), Ok(i64::MAX));
        assert_eq!(minor("-92233720368547758.08", "USD"), Ok(i64::MIN));
        assert_eq!(
            minor("999999999999999999.999", "KWD"),
            Err(MoneyError::Overflow)
        );
    }

    #[test]
    fn decimal_negation_flips_the_sign() {
        let amount = DecimalAmount::parse("12.30").unwrap();
        assert_eq!(amount.negated().as_str(), "-12.30");
        assert_eq!(amount.negated().negated().as_str(), "12.30");
        assert_eq!(amount.negated().to_minor_units(usd()), Ok(-1230));
        for zero in ["0", "0.00", "00"] {
            let zero = DecimalAmount::parse(zero).unwrap();
            assert_eq!(zero.negated().as_str(), zero.as_str(), "zero has no sign");
        }
    }

    #[test]
    fn rounding_to_minor_units_is_half_even() {
        let jpy = Currency::parse("JPY").unwrap();
        let rounded = |text: &str, currency| {
            DecimalAmount::parse(text)
                .unwrap()
                .to_minor_units_rounded(currency)
        };
        for (text, minor) in [
            ("23631.9805", 2_363_198),
            ("1.005", 100),
            ("1.015", 102),
            ("1.0051", 101),
            ("1.0049999", 100),
            ("1.009", 101),
            ("-1.005", -100),
            ("-1.015", -102),
            ("-1.0051", -101),
            ("-0.001", 0),
            ("12.3", 1230),
            ("12", 1200),
            ("0.995", 100),
        ] {
            assert_eq!(rounded(text, usd()), Ok(minor), "{text}");
        }
        assert_eq!(rounded("2.5", jpy), Ok(2));
        assert_eq!(rounded("3.5", jpy), Ok(4));
        assert_eq!(
            rounded("999999999999999999", usd()),
            Err(MoneyError::Overflow)
        );
        assert_eq!(rounded("92233720368547758.07", usd()), Ok(i64::MAX));
        assert_eq!(
            rounded("92233720368547758.075", usd()),
            Err(MoneyError::Overflow),
            "rounding up past the largest value overflows"
        );
    }

    #[test]
    fn format_minor_units_places_the_point() {
        assert_eq!(format_minor_units(-1234, usd()), "-12.34");
        assert_eq!(format_minor_units(5, usd()), "0.05");
        assert_eq!(format_minor_units(-5, usd()), "-0.05");
        assert_eq!(format_minor_units(0, usd()), "0.00");
        assert_eq!(
            format_minor_units(-7, Currency::parse("JPY").unwrap()),
            "-7"
        );
        assert_eq!(
            format_minor_units(1, Currency::parse("KWD").unwrap()),
            "0.001"
        );
        assert_eq!(format_minor_units(i64::MIN, usd()), "-92233720368547758.08");
    }

    #[test]
    fn errors_do_not_echo_input() {
        let secret = "https://user:secret@example.test";
        for err in [
            Currency::parse(secret).unwrap_err(),
            DecimalAmount::parse(secret).unwrap_err(),
        ] {
            assert!(!err.to_string().contains("secret"));
        }
    }

    proptest::proptest! {
        #[test]
        fn format_then_parse_roundtrips(
            minor in -1_000_000_000_000_000_i64..=1_000_000_000_000_000_i64,
            code in proptest::sample::select(vec!["JPY", "USD", "KWD"]),
        ) {
            let currency = Currency::parse(code).unwrap();
            let text = format_minor_units(minor, currency);
            let parsed = DecimalAmount::parse(&text).unwrap();
            proptest::prop_assert_eq!(parsed.as_str(), text.as_str());
            proptest::prop_assert_eq!(parsed.to_minor_units(currency), Ok(minor));
            proptest::prop_assert_eq!(parsed.to_minor_units_rounded(currency), Ok(minor));
        }
    }
}
