//! What to say when a bill settled as paid shares a category with a budget that spending
//! reduced (spec §8.9 R9a). Settled as paid, a bill's amount comes out of the category's
//! spending whether or not a transaction fits it, so the money past that is left to the budget,
//! and it may be the bill's too. Nothing else shows it but the budget's `reduced_by`, which reads
//! low with no caution. A settled bill is a person's decision, so this is a note on the answer
//! and never a caution: it cannot make a forecast `provisional`.

use super::reduction::{Reduction, pool_key};
use super::{ForecastError, ForecastInput, SettledBudget, money};
use crate::plan::CalendarMonth;

/// One note per budget the settlement's assumption lowered, in input order. A budget it lowered
/// by nothing has none.
pub(super) fn settled_budgets(
    input: &ForecastInput,
    reduction: &Reduction,
) -> Result<Vec<SettledBudget>, ForecastError> {
    let Ok(month) = CalendarMonth::of(input.as_of) else {
        return Ok(Vec::new());
    };
    let mut notes = Vec::new();
    for (item, adjustment) in input.items.iter().zip(&reduction.by_item) {
        let Some(key) = pool_key(item) else {
            continue;
        };
        if adjustment.reduced_by > 0 && reduction.assumed.contains(&key) {
            notes.push(SettledBudget {
                source: item.source,
                name: item.name.clone(),
                category: item.category,
                month,
                amount: money(adjustment.reduced_by, key.1)?,
            });
        }
    }
    Ok(notes)
}
