//! Dates in requests, for `#[serde(deserialize_with = "...")]`. A civil date's
//! own parser also takes a datetime, with or without an offset or a time zone,
//! and keeps only its date, so a caller who means another zone's day gets a
//! silently different one. Here a date is exactly `YYYY-MM-DD`. Refusals never
//! echo the input.

use jiff::civil::Date;
use serde::de::Error;
use serde::{Deserialize, Deserializer};

const REFUSAL: &str = "a date must be a real calendar date written YYYY-MM-DD";

/// Deserializes a required date.
///
/// # Errors
/// Refuses anything but a real date written `YYYY-MM-DD`.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse(&text).ok_or_else(|| D::Error::custom(REFUSAL))
}

/// Deserializes an optional date: `null` is none. Pair it with
/// `#[serde(default)]` so the field may also be absent.
///
/// # Errors
/// Refuses anything but `null` or a real date written `YYYY-MM-DD`.
pub fn deserialize_option<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Date>, D::Error> {
    Option::<String>::deserialize(deserializer)?
        .map(|text| parse(&text).ok_or_else(|| D::Error::custom(REFUSAL)))
        .transpose()
}

fn parse(text: &str) -> Option<Date> {
    let &[y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1] = text.as_bytes() else {
        return None;
    };
    if [y0, y1, y2, y3, m0, m1, d0, d1]
        .iter()
        .all(u8::is_ascii_digit)
    {
        text.parse().ok()
    } else {
        None
    }
}
