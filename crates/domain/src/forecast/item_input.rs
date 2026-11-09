//! Counting an item's occurrences inside the months its values are in force for.

use jiff::civil::Date;

use super::ItemInput;
use crate::plan::CalendarMonth;

impl ItemInput {
    /// The occurrences inside `[from, until_exclusive)`, ascending, clipped to the months this
    /// input's values are in force for. The forecast counts no occurrence any other way, so
    /// an item given as several inputs counts each date once.
    #[must_use]
    pub fn occurrences(&self, from: Date, until_exclusive: Date) -> Vec<Date> {
        let from = self
            .in_force
            .from
            .map_or(from, |start| from.max(start.first_day()));
        let until_exclusive = self
            .in_force
            .until
            .map_or(until_exclusive, |end| until_exclusive.min(end.first_day()));
        if from >= until_exclusive {
            return Vec::new();
        }
        self.schedule.occurrences(from, until_exclusive)
    }

    /// The occurrences in `month`, clipped the same way.
    #[must_use]
    pub fn occurrences_in(&self, month: CalendarMonth) -> Vec<Date> {
        self.occurrences(month.first_day(), month.next().first_day())
    }
}

impl ItemInput {
    /// The occurrences of the schedule alone, past the months this input is in force for. Only
    /// for telling which occurrence a payment is nearest, never for counting one.
    pub(super) fn neighbours(&self, from: Date, until_exclusive: Date) -> Vec<Date> {
        self.schedule.occurrences(from, until_exclusive)
    }

    /// The other amounts, in minor units, the item held during the month `on` falls in.
    pub(super) fn also_matches_on(&self, on: Date) -> impl Iterator<Item = i128> {
        CalendarMonth::of(on)
            .ok()
            .and_then(|month| self.also_matches.get(&month))
            .into_iter()
            .flatten()
            .map(|money| i128::from(money.minor_units))
    }
}
