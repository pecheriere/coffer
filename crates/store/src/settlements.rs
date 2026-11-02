//! Settlements of planned items (spec §8.9 R9a): one item's occurrences in one calendar
//! month, marked paid or skipped. A row goes with its item.

use coffer_domain::ids::{AgentTokenId, MemberId, PlannedItemId};
use coffer_domain::plan::{CalendarMonth, SettlementKind};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqliteExecutor};

use crate::convert::{parsed, parsed_opt, time};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

const ALL: &str = "SELECT * FROM planned_item_settlements ORDER BY item_id, month";
const OF_ITEM: &str = "SELECT * FROM planned_item_settlements WHERE item_id = ? ORDER BY month";

/// A stored settlement: which month of which item, and who said so when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementRecord {
    pub item: PlannedItemId,
    pub month: CalendarMonth,
    pub kind: SettlementKind,
    pub member: MemberId,
    /// The agent token the member acted through; `None` when signed in.
    pub agent: Option<AgentTokenId>,
    pub settled_at: Timestamp,
}

fn from_row(row: &SqliteRow) -> Result<SettlementRecord, StoreError> {
    Ok(SettlementRecord {
        item: parsed(row, "item_id")?,
        month: parsed(row, "month")?,
        kind: parsed(row, "kind")?,
        member: parsed(row, "settled_by_member")?,
        agent: parsed_opt(row, "settled_by_agent")?,
        settled_at: time(row, "settled_at")?,
    })
}

async fn of_item<'e>(
    executor: impl SqliteExecutor<'e>,
    item: PlannedItemId,
) -> Result<Vec<SettlementRecord>, StoreError> {
    sqlx::query(OF_ITEM)
        .bind(item.to_string())
        .fetch_all(executor)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(from_row)
        .collect()
}

/// Every settlement, ordered by item and month. At most `MAX_PLAN_ITEMS` items hold at most
/// `MAX_ITEM_SETTLEMENTS` each, which bounds it.
pub(crate) async fn all(conn: &mut SqliteConnection) -> Result<Vec<SettlementRecord>, StoreError> {
    sqlx::query(ALL)
        .fetch_all(conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(from_row)
        .collect()
}

impl StoreTx {
    /// `item`'s settlements, ordered by month, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn settlements_of_item(
        &mut self,
        item: PlannedItemId,
    ) -> Result<Vec<SettlementRecord>, StoreError> {
        of_item(&mut *self.tx, item).await
    }

    /// Writes a settlement, replacing the one its item already has for that month.
    ///
    /// # Errors
    ///
    /// Backend failures, including an item that does not exist.
    pub async fn put_settlement(&mut self, record: &SettlementRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO planned_item_settlements \
             (item_id, month, kind, settled_by_member, settled_by_agent, settled_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (item_id, month) DO UPDATE SET kind = excluded.kind, \
             settled_by_member = excluded.settled_by_member, \
             settled_by_agent = excluded.settled_by_agent, \
             settled_at = excluded.settled_at",
        )
        .bind(record.item.to_string())
        .bind(record.month.to_string())
        .bind(record.kind.as_str())
        .bind(record.member.to_string())
        .bind(record.agent.map(|agent| agent.to_string()))
        .bind(record.settled_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Deletes `item`'s settlement for `month`.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is none.
    pub async fn delete_settlement(
        &mut self,
        item: PlannedItemId,
        month: CalendarMonth,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("DELETE FROM planned_item_settlements WHERE item_id = ? AND month = ?")
                .bind(item.to_string())
                .bind(month.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }
}

impl Store {
    /// `item`'s settlements, ordered by month.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn settlements_of_item(
        &self,
        item: PlannedItemId,
    ) -> Result<Vec<SettlementRecord>, StoreError> {
        of_item(&self.pool, item).await
    }

    /// Every settlement, ordered by item and month.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn settlements(&self) -> Result<Vec<SettlementRecord>, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        all(&mut conn).await
    }
}
