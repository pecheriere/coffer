//! The periods a report answers for (spec §8.10 N12).

use std::str::FromStr;

use jiff::ToSpan;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::types::{NetWorthError, NetWorthValueError};
use crate::transactions::string_enum;

/// The most points one report answers.
pub const MAX_POINTS: usize = 1000;

string_enum!(
    /// How long a report's periods are.
    NetWorthInterval, NetWorthValueError::UnknownInterval,
    {
        Day => "day",
        /// Ending on Sunday.
        Week => "week",
        Month => "month",
    }
);

/// One period of a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodEnd {
    /// The day valued.
    pub on: Date,
    /// The first day of the period inside the range: never before `from`.
    pub start: Date,
    /// `on` is not the natural end of its period, because the range ends first.
    pub partial: bool,
}

/// The periods of `[from, to]`, oldest first.
///
/// # Errors
/// [`NetWorthError::RangeInverted`] when `from` is after `to`,
/// [`NetWorthError::TooManyPoints`] beyond [`MAX_POINTS`], and
/// [`NetWorthError::OutOfRange`] when a period ends beyond the calendar.
pub fn period_ends(
    from: Date,
    to: Date,
    interval: NetWorthInterval,
) -> Result<Vec<PeriodEnd>, NetWorthError> {
    if from > to {
        return Err(NetWorthError::RangeInverted);
    }
    let mut periods = Vec::new();
    let mut start = from;
    loop {
        if periods.len() == MAX_POINTS {
            return Err(NetWorthError::TooManyPoints);
        }
        let natural = natural_end(start, interval)?;
        let on = natural.min(to);
        periods.push(PeriodEnd {
            on,
            start,
            partial: on != natural,
        });
        if on == to {
            return Ok(periods);
        }
        start = on.tomorrow().map_err(|_| NetWorthError::OutOfRange)?;
    }
}

/// The last day of the period holding `day`.
fn natural_end(day: Date, interval: NetWorthInterval) -> Result<Date, NetWorthError> {
    match interval {
        NetWorthInterval::Day => Ok(day),
        NetWorthInterval::Week => {
            let days_to_sunday = (7 - day.weekday().to_sunday_zero_offset()) % 7;
            day.checked_add(i64::from(days_to_sunday).days())
                .map_err(|_| NetWorthError::OutOfRange)
        }
        NetWorthInterval::Month => Ok(day.last_of_month()),
    }
}
