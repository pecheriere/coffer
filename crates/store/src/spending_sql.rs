//! The SQL fragments that decide what counts as spending, shared by what a forecast reads and
//! what a plan versus actual report reads, and the readers for the sums they produce.

use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;

use crate::StoreError;
use crate::convert::{corrupt, int};

/// An account `a` of a class a household spends from. Invested and illiquid accounts
/// aren't, so their sweeps and trades don't drown what is.
macro_rules! spendable {
    () => {
        "a.liquidity IN ('cash', 'short_term_liability')"
    };
}
pub(crate) use spendable;

/// An account `a` whose money is the household's: what happens on any other is no part of
/// what the household spent (spec §8.11 B10).
macro_rules! the_households {
    () => {
        "a.belongs_to = 'household'"
    };
}
pub(crate) use the_households;

/// The transaction `t` still says what its splits say: its provider hasn't removed it or
/// revised it away from them.
macro_rules! splits_hold {
    () => {
        "t.flag_source_removed = 0 AND t.flag_split_mismatch = 0"
    };
}
pub(crate) use splits_hold;

/// A split `s` in a category `c` that adds to what its kind counts: money out of an
/// `expense` category, money into an `income` one.
macro_rules! adds_to_category {
    () => {
        "(c.kind = 'income') = (s.minor_units > 0)"
    };
}
pub(crate) use adds_to_category;

/// Money a category doesn't count (R10a). A charge the provider removed, or revised away
/// from its splits, doesn't say what was spent in a category, so it pays no bill and
/// reduces no budget. An inflow on such a transaction keeps counting: leaving a refund out
/// would raise the spending, the opposite of holding more.
///
/// The same goes for a deposit in an `income` category, which only a report reads: a
/// deposit the provider took back doesn't say what was earned.
macro_rules! left_out_of_category {
    () => {
        concat!("(", adds_to_category!(), " AND NOT (", splits_hold!(), "))")
    };
}
pub(crate) use left_out_of_category;

/// Money counted against a goal (R10a): a charge the provider removed or revised away
/// from its splits, or that may be another charge again, doesn't count. An inflow on such
/// a transaction keeps counting, since leaving a refund out would lower the need.
macro_rules! counts_against_goal {
    () => {
        concat!(
            "(s.minor_units > 0 OR (",
            splits_hold!(),
            " AND t.flag_possible_duplicate = 0))"
        )
    };
}
pub(crate) use counts_against_goal;

/// An outflow with no splits at all, in `t`, and something of it that didn't only move, by
/// `$counted`. Any other link leaves it here until it is filed: money to a 529 or a brokerage is
/// real spending (spec §8.11 B9), so it stays on a list.
macro_rules! uncategorized {
    ($counted:ident) => {
        concat!(
            "FILTER (WHERE t.amount_minor < 0 AND NOT EXISTS \
             (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id) AND ",
            after_moved!("t.amount_minor", $counted),
            " < 0)"
        )
    };
}
pub(crate) use uncategorized;

/// A summed amount. `SUM` that overflows fails its statement, but negating a sum of exactly
/// `i64::MIN` makes it a float instead, which is the same sum too large for an integer.
pub(crate) fn summed(row: &SqliteRow, column: &str) -> Result<i64, StoreError> {
    row.try_get(column).map_err(|err| match err {
        sqlx::Error::ColumnDecode { .. } => StoreError::SumTooLarge,
        other => StoreError::from_sqlx(other),
    })
}

pub(crate) fn count(row: &SqliteRow, column: &str) -> Result<u32, StoreError> {
    u32::try_from(int(row, column)?).map_err(|_| corrupt("a count out of range"))
}
