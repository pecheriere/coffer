//! What a person settled (spec §8.9 R9a): one item's occurrences in one calendar month, paid
//! some way the forecast can't see, or skipped. They count as nothing: no gross, no gap, no
//! share of the category's spending, and nothing for a caution to say. A months floor keeps
//! them, since it says how much to hold rather than what is owed. The answer lists them, so
//! no number silently differs, and they raise no caution of their own, since a person
//! decided.

use jiff::civil::Date;

use super::{ForecastError, ForecastInput, ItemInput, SettledLine, count, money};
use crate::plan::{CalendarMonth, occurrences_in};

/// Whether `item`'s occurrence on `on` is in a settled month.
pub(super) fn is_settled(item: &ItemInput, on: Date) -> bool {
    !item.settled.is_empty()
        && CalendarMonth::of(on).is_ok_and(|month| item.settled.contains_key(&month))
}

/// The lines of what was settled and would have counted: occurrences from the first of
/// `as_of`'s month, since one passed earlier in it would owe a gap or take spending, up to
/// the window's end.
pub(super) fn settled_lines(
    input: &ForecastInput,
    end_exclusive: Date,
) -> Result<Vec<SettledLine>, ForecastError> {
    let month_start = input.as_of.first_of_month();
    let mut lines = Vec::new();
    for item in &input.items {
        for (&month, &kind) in &item.settled {
            let mut dates = occurrences_in(&item.schedule, month);
            dates.retain(|on| (month_start..end_exclusive).contains(on));
            let occurrences = count(&dates)?;
            if occurrences == 0 {
                continue;
            }
            lines.push(SettledLine {
                source: item.source,
                name: item.name.clone(),
                item_type: item.schedule.item_type(),
                category: item.category,
                month,
                kind,
                occurrences,
                amount: money(
                    i128::from(item.amount.minor_units) * i128::from(occurrences),
                    item.amount.currency,
                )?,
            });
        }
    }
    Ok(lines)
}
