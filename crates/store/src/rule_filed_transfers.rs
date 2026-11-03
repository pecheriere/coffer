//! Transfers a rule filed with no link in a forecast's month (spec §8.12 T2): money out filed
//! under a `transfer` category by a rule, which only reports.

use coffer_domain::forecast::RuleFiledTransfers;
use coffer_domain::money::{Currency, Money};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteConnection};

use crate::convert::{corrupt, int, text};
use crate::error::StoreError;

/// By currency, for the bound `?1` day and the bound `?2` first day of its month. The splits a
/// rule filed under a `transfer` category on a transaction no link holds, whoever's account it is
/// on and whatever its state, as `GET /transactions?unmatched_transfer=true&rule=` lists them.
/// A category someone chose since isn't a rule's (`categorized_by_rule` is replaced with each
/// filing), and money in isn't counted.
const RULE_FILED_TRANSFERS: &str = "\
    SELECT t.currency AS currency, COUNT(DISTINCT t.id) AS count, \
    -SUM(s.minor_units) AS amount_minor \
    FROM transactions t \
    JOIN transaction_splits s ON s.transaction_id = t.id \
    JOIN categories c ON c.id = s.category_id \
    WHERE c.kind = 'transfer' AND s.minor_units < 0 \
    AND t.categorized_by_rule IS NOT NULL \
    AND t.posted_on >= ?2 AND t.posted_on <= ?1 \
    AND NOT EXISTS (SELECT 1 FROM transfer_links l \
    WHERE l.outflow_id = t.id OR l.inflow_id = t.id) \
    GROUP BY t.currency ORDER BY t.currency";

/// The rows of [`RULE_FILED_TRANSFERS`].
pub(crate) async fn read(
    conn: &mut SqliteConnection,
    as_of: &str,
    month_start: &str,
) -> Result<Vec<RuleFiledTransfers>, StoreError> {
    sqlx::query(RULE_FILED_TRANSFERS)
        .bind(as_of)
        .bind(month_start)
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(from_row)
        .collect()
}

fn from_row(row: &SqliteRow) -> Result<RuleFiledTransfers, StoreError> {
    let currency = Currency::parse(&text(row, "currency")?).map_err(StoreError::backend)?;
    let minor_units = row.try_get("amount_minor").map_err(|err| match err {
        sqlx::Error::ColumnDecode { .. } => StoreError::SumTooLarge,
        other => StoreError::from_sqlx(other),
    })?;
    Ok(RuleFiledTransfers {
        currency,
        count: u32::try_from(int(row, "count")?).map_err(|_| corrupt("a count out of range"))?,
        amount: Money {
            minor_units,
            currency,
        },
    })
}
