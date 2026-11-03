//! Transfer links: which two transactions are the legs of one movement of money (spec §8.12).
//! What a link means for a count is defined here once, and read where the count is, in
//! [`crate::forecast`].

use std::collections::HashMap;

use coffer_domain::ids::{TransactionId, TransferLinkId};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};

use crate::convert::{int, parsed, parsed_opt, time};
use crate::members::expect_one;
use crate::transactions::Attribution;
use crate::{Store, StoreError, StoreTx};

/// The account under this alias is in the cash the forecast counts: open, switched in and of a
/// spendable class. A flag can't be set on an account that isn't the household's (§8.11 B4).
macro_rules! counted_cash {
    ($a:literal) => {
        concat!(
            $a,
            ".closed_at IS NULL AND ",
            $a,
            ".include_in_forecast = 1 AND ",
            $a,
            ".liquidity IN ('cash', 'short_term_liability')"
        )
    };
}
pub(crate) use counted_cash;

/// A link `lk` with its legs `lo` and `li` and their accounts `ao` and `ai`.
macro_rules! link_legs {
    () => {
        "transfer_links lk \
         JOIN transactions lo ON lo.id = lk.outflow_id \
         JOIN accounts ao ON ao.id = lo.account_id \
         JOIN transactions li ON li.id = lk.inflow_id \
         JOIN accounts ai ON ai.id = li.account_id"
    };
}
pub(crate) use link_legs;

/// What the legs say still fits a link: one out and one in, and neither taken back by its
/// provider (spec §8.12 T7).
macro_rules! link_stands {
    () => {
        "lo.amount_minor < 0 AND li.amount_minor > 0 \
         AND lo.flag_source_removed = 0 AND li.flag_source_removed = 0"
    };
}
pub(crate) use link_stands;

/// Both accounts are in counted cash, so money between them left no cash (spec §8.12 T5).
macro_rules! both_counted {
    () => {
        concat!(counted_cash!("ao"), " AND ", counted_cash!("ai"))
    };
}
pub(crate) use both_counted;

/// As [`both_counted`], for a report, which says what happened: an account closed later was
/// counted when its leg posted, so closing it says nothing about January, nor about the last sweep
/// before it closed. A leg counts when `posted_on` is on or before the UTC date of `closed_at` plus
/// one day. That isn't worked out in the household's zone: the extra day only covers a `posted_on`
/// that is ahead of the UTC date. The flags and the class are still read at the time (§8.11 B6).
macro_rules! both_counted_when_posted {
    () => {
        concat!(
            counted_when_posted!("ao", "lo"),
            " AND ",
            counted_when_posted!("ai", "li")
        )
    };
}
pub(crate) use both_counted_when_posted;

/// The account under the alias `$a` as [`both_counted_when_posted`] reads it for its leg `$leg`.
macro_rules! counted_when_posted {
    ($a:literal, $leg:literal) => {
        concat!(
            $a,
            ".include_in_forecast = 1 AND ",
            $a,
            ".liquidity IN ('cash', 'short_term_liability') AND (",
            $a,
            ".closed_at IS NULL OR ",
            $leg,
            ".posted_on <= date(",
            $a,
            ".closed_at / 1000, 'unixepoch', '+1 day'))"
        )
    };
}
pub(crate) use counted_when_posted;

/// Whether the link `l` is one movement, by `$counted`.
macro_rules! one_movement {
    ($counted:ident) => {
        concat!(
            "EXISTS (SELECT 1 FROM ",
            link_legs!(),
            " WHERE lk.id = l.id AND ",
            link_stands!(),
            " AND ",
            $counted!(),
            ")"
        )
    };
}

/// What of the leg `t` a link says moved: all of the smaller leg, so a difference stays what it
/// is filed as. Across currencies this doesn't read an exchange rate, so all of each leg moved,
/// which is a known limit (spec §8.12 T5).
macro_rules! link_amount {
    () => {
        "CASE WHEN lo.currency = li.currency \
         THEN MIN(-lo.amount_minor, li.amount_minor) ELSE ABS(t.amount_minor) END"
    };
}
pub(crate) use link_amount;

/// The magnitude of the leg `t` that is one movement between counted cash, by `$counted`, and 0
/// for any other transaction: the one definition behind every number in spec §8.12 T5 and T6. The
/// forecast passes [`both_counted`] and a report [`both_counted_when_posted`].
macro_rules! moved_minor {
    ($counted:ident) => {
        concat!(
            "COALESCE((SELECT ",
            link_amount!(),
            " FROM ",
            link_legs!(),
            " WHERE (lk.outflow_id = t.id OR lk.inflow_id = t.id) AND ",
            link_stands!(),
            " AND ",
            $counted!(),
            "), 0)"
        )
    };
}
pub(crate) use moved_minor;

/// `$value`, a signed amount on `t`, less what of it moved.
macro_rules! after_moved {
    ($value:literal, $counted:ident) => {
        concat!(
            "(",
            $value,
            " - MAX(-",
            moved_minor!($counted),
            ", MIN(",
            moved_minor!($counted),
            ", ",
            $value,
            ")))"
        )
    };
}
pub(crate) use after_moved;

/// The links one movement but for a leg its provider took back, with a leg the look-back reaches:
/// their money counts again as it is filed, and a person should look (spec §8.12 T7).
const LINKS_WITH_LEG_REMOVED: &str = concat!(
    "SELECT lk.id AS link FROM ",
    link_legs!(),
    " WHERE (lo.flag_source_removed = 1 OR li.flag_source_removed = 1) AND ",
    both_counted!(),
    " AND ((lo.posted_on >= ?2 AND lo.posted_on <= ?1) \
     OR (li.posted_on >= ?2 AND li.posted_on <= ?1)) ORDER BY lk.id"
);

/// See [`LINKS_WITH_LEG_REMOVED`].
pub(crate) async fn links_with_leg_removed(
    conn: &mut SqliteConnection,
    as_of: &str,
    looked_at_from: &str,
) -> Result<Vec<TransferLinkId>, StoreError> {
    sqlx::query(LINKS_WITH_LEG_REMOVED)
        .bind(as_of)
        .bind(looked_at_from)
        .fetch_all(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?
        .iter()
        .map(|row| parsed(row, "link"))
        .collect()
}

/// A stored link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferLinkRecord {
    pub id: TransferLinkId,
    /// The leg with money going out.
    pub outflow: TransactionId,
    /// The leg with money coming in.
    pub inflow: TransactionId,
    /// Both accounts are in counted cash and the link still stands, so the forecast takes what
    /// moved as one movement.
    pub one_movement: bool,
    /// The same as a report reads it: an account closed since its leg posted still counts.
    pub one_movement_in_reports: bool,
    /// Who made it and when.
    pub created: Attribution,
}

/// A link as one of its legs sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferLinkSide {
    pub id: TransferLinkId,
    /// The other leg.
    pub other: TransactionId,
    /// As [`TransferLinkRecord::one_movement`].
    pub one_movement: bool,
    /// As [`TransferLinkRecord::one_movement_in_reports`].
    pub one_movement_in_reports: bool,
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
        one_movement: int(row, "one_movement")? == 1,
        one_movement_in_reports: int(row, "one_movement_in_reports")? == 1,
        created: Attribution {
            member: Some(parsed(row, "created_by_member")?),
            agent: parsed_opt(row, "created_by_agent")?,
            system: None,
            rule: None,
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
        one_movement!(both_counted),
        " AS one_movement, ",
        one_movement!(both_counted_when_posted),
        " AS one_movement_in_reports FROM transfer_links l WHERE l.id = ?"
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
                one_movement!(both_counted),
                " AS one_movement, ",
                one_movement!(both_counted_when_posted),
                " AS one_movement_in_reports FROM transfer_links l WHERE l."
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
                    one_movement: int(row, "one_movement")? == 1,
                    one_movement_in_reports: int(row, "one_movement_in_reports")? == 1,
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

    /// Whether a link between these two transactions would be one movement in a report, which
    /// holds wherever it would in the forecast.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn would_be_one_movement(
        &mut self,
        outflow: TransactionId,
        inflow: TransactionId,
    ) -> Result<bool, StoreError> {
        let found = sqlx::query(concat!(
            "SELECT 1 FROM transactions lo JOIN accounts ao ON ao.id = lo.account_id, \
             transactions li JOIN accounts ai ON ai.id = li.account_id \
             WHERE lo.id = ? AND li.id = ? AND ",
            both_counted_when_posted!()
        ))
        .bind(outflow.to_string())
        .bind(inflow.to_string())
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(found.is_some())
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
