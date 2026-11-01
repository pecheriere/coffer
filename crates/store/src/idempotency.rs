//! Idempotency keys: a create made with a key happens once, and a retry
//! with the same key and request gets the first response back.
//!
//! A key is reserved (`pending`) in its own transaction, then completed in
//! the transaction that creates the resource, so the record and the
//! resource commit or roll back together.

use coffer_domain::ids::MemberId;
use jiff::{SignedDuration, Timestamp};
use sqlx::Row;

use crate::convert::{blob, text, time};
use crate::{StoreError, StoreTx};

/// How long a key is remembered.
const KEY_LIFETIME: SignedDuration = SignedDuration::from_hours(24);

/// How long a reservation may stay pending before it counts as abandoned.
/// The create that reserved it rolled back (its completion would have
/// committed with it), so another request may take the key over.
const PENDING_LIFETIME: SignedDuration = SignedDuration::from_mins(5);

/// The outcome of reserving a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    /// The key is now reserved for this request: perform the create.
    Fresh,
    /// The same request completed already: answer with its response.
    Replay { status: u16, response_json: String },
    /// The key was used for a different request.
    Mismatch,
    /// The same request is still being performed.
    InProgress,
}

impl StoreTx {
    /// Reserve `key` for `member`'s request whose hash is `hash`. Keys
    /// older than 24 hours are purged first.
    ///
    /// # Errors
    ///
    /// Backend failures, or a stored row that no longer parses.
    pub async fn reserve_idempotency(
        &mut self,
        member: MemberId,
        key: &str,
        hash: [u8; 32],
        now: Timestamp,
    ) -> Result<Reservation, StoreError> {
        let expired = now.checked_sub(KEY_LIFETIME).map_err(StoreError::backend)?;
        sqlx::query("DELETE FROM idempotency_keys WHERE created_at < ?")
            .bind(expired.as_millisecond())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;

        let row = sqlx::query(
            "SELECT request_hash, state, status, response_json, created_at \
             FROM idempotency_keys WHERE member_id = ? AND key = ?",
        )
        .bind(member.to_string())
        .bind(key)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        let Some(row) = row else {
            sqlx::query(
                "INSERT INTO idempotency_keys (member_id, key, request_hash, state, created_at) \
                 VALUES (?, ?, ?, 'pending', ?)",
            )
            .bind(member.to_string())
            .bind(key)
            .bind(hash.as_slice())
            .bind(now.as_millisecond())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
            return Ok(Reservation::Fresh);
        };

        if blob(&row, "request_hash")? != hash {
            return Ok(Reservation::Mismatch);
        }
        match text(&row, "state")?.as_str() {
            "done" => {
                let status: i64 = row.try_get("status").map_err(StoreError::from_sqlx)?;
                Ok(Reservation::Replay {
                    status: u16::try_from(status).map_err(StoreError::backend)?,
                    response_json: text(&row, "response_json")?,
                })
            }
            "pending" => {
                let abandoned_before = now
                    .checked_sub(PENDING_LIFETIME)
                    .map_err(StoreError::backend)?;
                if time(&row, "created_at")? >= abandoned_before {
                    return Ok(Reservation::InProgress);
                }
                sqlx::query(
                    "UPDATE idempotency_keys SET created_at = ? \
                     WHERE member_id = ? AND key = ? AND state = 'pending'",
                )
                .bind(now.as_millisecond())
                .bind(member.to_string())
                .bind(key)
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
                Ok(Reservation::Fresh)
            }
            _ => Err(crate::convert::corrupt("unknown idempotency key state")),
        }
    }

    /// Record the response of the create that reserved `key`. Call it in the
    /// create's own transaction, so it commits or rolls back with it.
    ///
    /// # Errors
    ///
    /// `Conflict` if `key` is not reserved (pending) for `member`; backend
    /// failures.
    pub async fn complete_idempotency(
        &mut self,
        member: MemberId,
        key: &str,
        status: u16,
        response_json: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE idempotency_keys SET state = 'done', status = ?, response_json = ? \
             WHERE member_id = ? AND key = ? AND state = 'pending'",
        )
        .bind(i64::from(status))
        .bind(response_json)
        .bind(member.to_string())
        .bind(key)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Conflict("idempotency key not reserved"));
        }
        Ok(())
    }

    /// Give up the reservation of `key` after its create failed, so a retry
    /// is performed rather than refused as in progress. A completed key is
    /// never removed.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn release_idempotency(
        &mut self,
        member: MemberId,
        key: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM idempotency_keys WHERE member_id = ? AND key = ? AND state = 'pending'",
        )
        .bind(member.to_string())
        .bind(key)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}
