//! The current month's reduction (spec §6.8, §8.9 R9).
//!
//! Each category and currency has one pool: this month's actual spending, less
//! the planned outflows dated earlier this month, since that spending was already
//! planned for, floored at 0. The pool then pays for this month's remaining
//! recurring occurrences in date order, so one purchase never reduces two items.
//! Only stored items take part: hypothetical ones neither reduce nor consume.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Span;
use jiff::civil::Date;

use super::count;
use super::types::{ForecastError, ForecastInput, ItemInput, LineSource};
use crate::ids::CategoryId;
use crate::money::Currency;
use crate::plan::{PlanDirection, PlanSchedule};

pub(super) struct Reduction {
    /// What comes off each item, by its position in the input.
    pub(super) by_item: Vec<i128>,
    /// The currencies with a recurring occurrence left to reduce this month:
    /// only there can this month's unsettled spending change the number.
    pub(super) reducible_currencies: BTreeSet<Currency>,
}

/// A stored outflow's category, the only kind of item that shares a pool.
fn pool_key(item: &ItemInput) -> Option<(CategoryId, Currency)> {
    let stored = matches!(item.source, LineSource::PlannedItem { .. });
    let outflow = item.direction == PlanDirection::Outflow;
    let category = item.category.filter(|_| stored && outflow)?;
    Some((category, item.amount.currency))
}

pub(super) fn reduce(
    input: &ForecastInput,
    end_exclusive: Date,
) -> Result<Reduction, ForecastError> {
    let month_start = input.as_of.first_of_month();
    let reducible_until = month_start
        .checked_add(Span::new().months(1))
        .map_or(end_exclusive, |next_month| next_month.min(end_exclusive));

    let mut pools: BTreeMap<(CategoryId, Currency), i128> = BTreeMap::new();
    for spent in &input.category_spent {
        *pools.entry((spent.category, spent.currency)).or_default() +=
            i128::from(spent.spent_minor);
    }

    let mut remaining = Vec::new();
    let mut reducible_currencies = BTreeSet::new();
    for (index, item) in input.items.iter().enumerate() {
        let Some(key) = pool_key(item) else {
            continue;
        };
        let amount = i128::from(item.amount.minor_units);
        if let Some(pool) = pools.get_mut(&key) {
            let earlier = item.schedule.occurrences(month_start, input.as_of);
            *pool -= amount * i128::from(count(&earlier)?);
        }
        if matches!(item.schedule, PlanSchedule::Recurring { .. }) {
            let dates = item.schedule.occurrences(input.as_of, reducible_until);
            if !dates.is_empty() {
                reducible_currencies.insert(item.amount.currency);
            }
            remaining.extend(dates.into_iter().map(|on| (on, index, key, amount)));
        }
    }

    // Stable, so items on the same day take from the pool in input order.
    remaining.sort_by_key(|&(on, ..)| on);
    let mut by_item = vec![0; input.items.len()];
    for (_, index, key, amount) in remaining {
        let (Some(pool), Some(reduced)) = (pools.get_mut(&key), by_item.get_mut(index)) else {
            continue;
        };
        let taken = amount.min((*pool).max(0));
        *pool -= taken;
        *reduced += taken;
    }
    Ok(Reduction {
        by_item,
        reducible_currencies,
    })
}
