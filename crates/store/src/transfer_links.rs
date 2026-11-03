//! Transfer links: which two transactions are the legs of one movement of money (spec §8.12).
//! What a link means for a count is read where the count is, in [`crate::forecast`].

use std::collections::HashMap;

use coffer_domain::ids::{TransactionId, TransferLinkId};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::convert::{parsed, parsed_opt, time};
use crate::members::expect_one;
use crate::transactions::Attribution;
use crate::{Store, StoreError, StoreTx};

/// Whose money both legs are, read at the time, since `belongs_to` can change.
macro_rules! within_household {
    () => {
        "EXISTS (SELECT 1 FROM transactions o JOIN accounts ao ON ao.id = o.account_id \
         JOIN transactions i JOIN accounts ai ON ai.id = i.account_id \
         WHERE o.id = l.outflow_id AND i.id = l.inflow_id \
         AND ao.belongs_to = 'household' AND ai.belongs_to = 'household')"
    };
}

/// A stored link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferLinkRecord {
    pub id: TransferLinkId,
    /// The leg with money going out.
    pub outflow: TransactionId,
    /// The leg with money coming in.
    pub inflow: TransactionId,
    /// Both legs are on the household's accounts, so reports treat them as one movement.
    pub within_household: bool,
    /// Who made it and when.
    pub created: Attribution,
}

/// A link as one of its legs sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferLinkSide {
    pub id: TransferLinkId,
    /// The other leg.
    pub other: TransactionId,
    /// Both legs are on the household's accounts.
    pub within_household: bool,
}

/// A link to create. The caller has checked the legs.
#[derive(Debug, Clone)]
pub struct NewTransferLink {
    pub outflow: TransactionId,
    pub inflow: TransactionId,
    /// A member, possibly through an agent token.
    pub by: Attribution,
}

fn link_from_row(row: &SqliteRow) -> Result<TransferLinkRecord, StoreError> {
    let at = time(row, "created_at")?;
    Ok(TransferLinkRecord {
        id: parsed(row, "id")?,
        outflow: parsed(row, "outflow_id")?,
        inflow: parsed(row, "inflow_id")?,
        within_household: crate::convert::int(row, "within_household")? == 1,
        created: Attribution {
            member: Some(parsed(row, "created_by_member")?),
            agent: parsed_opt(row, "created_by_agent")?,
            system: None,
            at,
        },
    })
}

async fn fetch_link(
    conn: &mut SqliteConnection,
    id: TransferLinkId,
) -> Result<TransferLinkRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT l.*, ",
        within_household!(),
        " AS within_household FROM transfer_links l WHERE l.id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    link_from_row(&row)
}

/// The link of each of `ids` that is a leg of one.
pub(crate) async fn sides_by_transaction(
    conn: &mut SqliteConnection,
    ids: &[TransactionId],
) -> Result<HashMap<TransactionId, TransferLinkSide>, StoreError> {
    let mut sides = HashMap::new();
    if ids.is_empty() {
        return Ok(sides);
    }
    for (own, other) in [("outflow_id", "inflow_id"), ("inflow_id", "outflow_id")] {
        let mut query = QueryBuilder::<Sqlite>::new("SELECT l.id AS id, l.");
        query
            .push(own)
            .push(" AS own, l.")
            .push(other)
            .push(concat!(
                " AS other, ",
                within_household!(),
                " AS within_household FROM transfer_links l WHERE l."
            ))
            .push(own)
            .push(" IN (");
        let mut separated = query.separated(", ");
        for id in ids {
            separated.push_bind(id.to_string());
        }
        separated.push_unseparated(")");
        let rows = query
            .build()
            .fetch_all(&mut *conn)
            .await
            .map_err(StoreError::from_sqlx)?;
        for row in &rows {
            sides.insert(
                parsed(row, "own")?,
                TransferLinkSide {
                    id: parsed(row, "id")?,
                    other: parsed(row, "other")?,
                    within_household: crate::convert::int(row, "within_household")? == 1,
                },
            );
        }
    }
    Ok(sides)
}

impl StoreTx {
    /// Create a link and return it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] if a leg is already in that place in a link; backend failures,
    /// including a leg in a link on its other side, or a transaction that does not exist.
    pub async fn insert_transfer_link(
        &mut self,
        new: NewTransferLink,
    ) -> Result<TransferLinkRecord, StoreError> {
        let id = TransferLinkId::new();
        sqlx::query(
            "INSERT INTO transfer_links \
             (id, outflow_id, inflow_id, created_by_member, created_by_agent, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.outflow.to_string())
        .bind(new.inflow.to_string())
        .bind(new.by.member.map(|member| member.to_string()))
        .bind(new.by.agent.map(|agent| agent.to_string()))
        .bind(new.by.at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        fetch_link(&mut self.tx, id).await
    }

    /// End a link. Its legs stay as they are.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such link.
    pub async fn delete_transfer_link(&mut self, id: TransferLinkId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM transfer_links WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Fetch a link, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such link.
    pub async fn get_transfer_link(
        &mut self,
        id: TransferLinkId,
    ) -> Result<TransferLinkRecord, StoreError> {
        fetch_link(&mut self.tx, id).await
    }
}

impl Store {
    /// Fetch a link.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such link.
    pub async fn get_transfer_link(
        &self,
        id: TransferLinkId,
    ) -> Result<TransferLinkRecord, StoreError> {
        let mut conn = self.pool.acquire().await.map_err(StoreError::from_sqlx)?;
        fetch_link(&mut conn, id).await
    }
}
