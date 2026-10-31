//! Login sessions. Only the hash of the session token is stored.

use coffer_domain::ids::{MemberId, SessionId};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, time};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

/// A stored session. The token hash is never read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub member: MemberId,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub idle_expires_at: Timestamp,
    pub absolute_expires_at: Timestamp,
}

/// A session to create.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub token_hash: [u8; 32],
    pub member: MemberId,
    pub created_at: Timestamp,
    pub idle_expires_at: Timestamp,
    pub absolute_expires_at: Timestamp,
}

impl StoreTx {
    /// Create a session.
    ///
    /// # Errors
    ///
    /// `Conflict` if the token hash already exists.
    pub async fn insert_session(&mut self, new: NewSession) -> Result<SessionRecord, StoreError> {
        let id = SessionId::new();
        sqlx::query(
            "INSERT INTO sessions \
             (id, token_hash, member_id, created_at, last_seen_at, idle_expires_at, \
              absolute_expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.token_hash.as_slice())
        .bind(new.member.to_string())
        .bind(new.created_at.as_millisecond())
        .bind(new.created_at.as_millisecond())
        .bind(new.idle_expires_at.as_millisecond())
        .bind(new.absolute_expires_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(SessionRecord {
            id,
            member: new.member,
            created_at: new.created_at,
            last_seen_at: new.created_at,
            idle_expires_at: new.idle_expires_at,
            absolute_expires_at: new.absolute_expires_at,
        })
    }

    /// Record activity and slide the idle expiry.
    ///
    /// # Errors
    ///
    /// `NotFound` if the session does not exist.
    pub async fn touch_session(
        &mut self,
        id: SessionId,
        now: Timestamp,
        new_idle_expires_at: Timestamp,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("UPDATE sessions SET last_seen_at = ?, idle_expires_at = ? WHERE id = ?")
                .bind(now.as_millisecond())
                .bind(new_idle_expires_at.as_millisecond())
                .bind(id.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete one session.
    ///
    /// # Errors
    ///
    /// `NotFound` if the session does not exist.
    pub async fn delete_session(&mut self, id: SessionId) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete every session of a member (zero is fine).
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn delete_sessions_for_member(&mut self, member: MemberId) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE member_id = ?")
            .bind(member.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}

fn session_from_row(row: &SqliteRow) -> Result<SessionRecord, StoreError> {
    Ok(SessionRecord {
        id: parsed(row, "id")?,
        member: parsed(row, "member_id")?,
        created_at: time(row, "created_at")?,
        last_seen_at: time(row, "last_seen_at")?,
        idle_expires_at: time(row, "idle_expires_at")?,
        absolute_expires_at: time(row, "absolute_expires_at")?,
    })
}

impl Store {
    /// Find a session that has neither idled out nor passed its absolute
    /// lifetime.
    ///
    /// # Errors
    ///
    /// `NotFound` when `now >= min(idle_expires_at, absolute_expires_at)`.
    pub async fn find_live_session(
        &self,
        token_hash: &[u8; 32],
        now: Timestamp,
    ) -> Result<SessionRecord, StoreError> {
        let row = sqlx::query(
            "SELECT id, member_id, created_at, last_seen_at, idle_expires_at, \
             absolute_expires_at FROM sessions \
             WHERE token_hash = ? AND idle_expires_at > ? AND absolute_expires_at > ?",
        )
        .bind(token_hash.as_slice())
        .bind(now.as_millisecond())
        .bind(now.as_millisecond())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        session_from_row(&row)
    }
}
