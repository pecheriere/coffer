//! One account's value on each day: the one function behind incremental
//! updates, the daily fill and the full rebuild (spec §8.10 N3).

use jiff::ToSpan;
use jiff::civil::{Date, date};

use super::types::ConversionState;
use crate::accounts::BalanceSource;
use crate::fx::{Conversion, RateBook, convert};
use crate::ids::AccountId;
use crate::money::Currency;

/// No row is earlier than this, so a snapshot dated in the distant past can't
/// make hundreds of thousands of them.
pub const MIN_VALUE_DATE: Date = date(1970, 1, 1);

/// A balance as of its day, in the account's currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPoint {
    pub on: Date,
    pub balance_minor: i64,
    pub source: BalanceSource,
}

/// One account on one day. Derived: it can be dropped and rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DailyValue {
    pub account: AccountId,
    pub on: Date,
    /// In the account's currency.
    pub balance_minor: i64,
    /// The date of the snapshot it comes from.
    pub balance_on: Date,
    pub balance_source: BalanceSource,
    /// `None` only with [`ConversionState::NoDisplayCurrency`].
    pub display: Option<Currency>,
    /// Present only with [`ConversionState::Same`] and
    /// [`ConversionState::Converted`].
    pub display_minor: Option<i64>,
    /// Present only with [`ConversionState::Converted`].
    pub rate_on: Option<Date>,
    pub conversion: ConversionState,
}

/// One row per day of `[max(from, first snapshot, MIN_VALUE_DATE), through]`,
/// from the latest snapshot on or before it. `snapshots`, in any order, holds
/// the latest one before `from`, if any, and those up to `through`.
#[must_use]
pub fn daily_values(
    account: AccountId,
    currency: Currency,
    snapshots: &[SnapshotPoint],
    display: Option<Currency>,
    book: &RateBook,
    from: Date,
    through: Date,
) -> Vec<DailyValue> {
    let mut sorted = snapshots.to_vec();
    sorted.sort_by_key(|snapshot| snapshot.on);
    let Some(first) = sorted.first() else {
        return Vec::new();
    };
    let start = from.max(first.on).max(MIN_VALUE_DATE);
    let mut rows = Vec::new();
    for on in start.series(1.day()).take_while(|on| *on <= through) {
        let upto = sorted.partition_point(|snapshot| snapshot.on <= on);
        let Some(snapshot) = upto.checked_sub(1).and_then(|last| sorted.get(last)) else {
            continue;
        };
        let (display_minor, rate_on, conversion) =
            display_amount(snapshot.balance_minor, currency, display, on, book);
        rows.push(DailyValue {
            account,
            on,
            balance_minor: snapshot.balance_minor,
            balance_on: snapshot.on,
            balance_source: snapshot.source,
            display,
            display_minor,
            rate_on,
            conversion,
        });
    }
    rows
}

fn display_amount(
    minor: i64,
    currency: Currency,
    display: Option<Currency>,
    on: Date,
    book: &RateBook,
) -> (Option<i64>, Option<Date>, ConversionState) {
    let Some(display) = display else {
        return (None, None, ConversionState::NoDisplayCurrency);
    };
    match convert(minor, currency, display, on, book) {
        Conversion::Same { minor } => (Some(minor), None, ConversionState::Same),
        Conversion::Converted { minor, rate_on } => {
            (Some(minor), Some(rate_on), ConversionState::Converted)
        }
        Conversion::Missing => (None, None, ConversionState::Missing),
        Conversion::TooLarge => (None, None, ConversionState::TooLarge),
    }
}
