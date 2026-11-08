//! The forecast's running amounts: the window it looks at, its month buckets, and the book of
//! each currency's tallies that every line adds to.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;

use super::{ForecastError, LineSource, MonthItem, money};
use crate::ids::AccountId;
use crate::money::Currency;
use crate::plan::CalendarMonth;

/// The days a forecast looks at.
pub(super) struct Window<'a> {
    pub(super) as_of: Date,
    pub(super) end_exclusive: Date,
    /// Where planned inflows stop: never past the same day `horizon_months` on, so
    /// the clamped day adds no income.
    pub(super) income_end_exclusive: Date,
    pub(super) buckets: &'a [Bucket],
    /// The accounts that count as cash: income expected anywhere else may never reach them.
    pub(super) counted: &'a BTreeSet<AccountId>,
}

/// The part of the window in one calendar month.
pub(super) struct Bucket {
    pub(super) month: CalendarMonth,
    pub(super) partial: bool,
    /// Where this bucket stops: the next month's first day or the window's end.
    pub(super) end_exclusive: Date,
}

pub(super) fn buckets(as_of: Date, end_exclusive: Date) -> Result<Vec<Bucket>, ForecastError> {
    let mut month = CalendarMonth::of(as_of).map_err(|_| ForecastError::OutOfRange)?;
    let mut start = as_of;
    let mut found = Vec::new();
    while start < end_exclusive {
        let next = month.next().first_day();
        // The calendar's last month is its own successor, and ends the window.
        let stop = if next > start {
            next.min(end_exclusive)
        } else {
            end_exclusive
        };
        found.push(Bucket {
            month,
            partial: start != month.first_day() || stop != next,
            end_exclusive: stop,
        });
        start = stop;
        month = month.next();
    }
    Ok(found)
}

/// One currency's running amounts. A row exists exactly for a currency in use.
pub(super) struct Tally {
    pub(super) cash: i128,
    pub(super) outflows: i128,
    pub(super) goals: i128,
    /// Every planned inflow in the window, counted or not, but for one expected in an account
    /// that isn't the household's.
    pub(super) inflows: i128,
    /// The emergency fund to keep untouched, from the currency's target.
    pub(super) emergency_fund: i128,
    pub(super) months: Vec<MonthTally>,
}

#[derive(Default)]
pub(super) struct MonthTally {
    pub(super) outflows: i128,
    pub(super) inflows: i128,
    pub(super) goal_accrual: i128,
    pub(super) items: Vec<MonthItem>,
}

impl MonthTally {
    pub(super) fn itemize(
        &mut self,
        source: LineSource,
        minor_units: i128,
        currency: Currency,
    ) -> Result<(), ForecastError> {
        if minor_units != 0 {
            self.items.push(MonthItem {
                source,
                amount: money(minor_units, currency)?,
            });
        }
        Ok(())
    }
}

pub(super) struct Book {
    pub(super) bucket_count: usize,
    pub(super) tallies: BTreeMap<Currency, Tally>,
}

impl Book {
    pub(super) fn new(bucket_count: usize) -> Self {
        Self {
            bucket_count,
            tallies: BTreeMap::new(),
        }
    }

    pub(super) fn tally(&mut self, currency: Currency) -> &mut Tally {
        let bucket_count = self.bucket_count;
        self.tallies.entry(currency).or_insert_with(|| Tally {
            cash: 0,
            outflows: 0,
            goals: 0,
            inflows: 0,
            emergency_fund: 0,
            months: (0..bucket_count).map(|_| MonthTally::default()).collect(),
        })
    }
}
