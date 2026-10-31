//! Registered passkeys.

use coffer_domain::ids::{MemberId, PasskeyId};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::convert::{blob, parsed, text, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

/// A stored passkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRecord {
    pub id: PasskeyId,
    pub member: MemberId,
    pub label: String,
    pub credential_id: Vec<u8>,
    pub credential_json: String,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
}

/// A passkey to register.
#[derive(Debug, Clone)]
pub struct NewPasskey {
    pub member: MemberId,
    pub label: String,
    pub credential_id: Vec<u8>,
    pub credential_json: String,
    pub created_at: Timestamp,
}

fn passkey_from_row(row: &SqliteRow) -> Result<PasskeyRecord, StoreError> {
    Ok(PasskeyRecord {
        id: parsed(row, "id")?,
        member: parsed(row, "member_id")?,
        label: text(row, "label")?,
        credential_id: blob(row, "credential_id")?,
        credential_json: text(row, "credential_json")?,
        created_at: time(row, "created_at")?,
        last_used_at: time_opt(row, "last_used_at")?,
    })
}

impl StoreTx {
    /// Register a passkey for a member.
    ///
    /// # Errors
    ///
    /// `Conflict` if the credential id is already registered.
    pub async fn insert_passkey(&mut self, new: NewPasskey) -> Result<PasskeyRecord, StoreError> {
        let id = PasskeyId::new();
        sqlx::query(
            "INSERT INTO passkeys \
             (id, member_id, label, credential_id, credential_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.member.to_string())
        .bind(&new.label)
        .bind(&new.credential_id)
        .bind(&new.credential_json)
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(PasskeyRecord {
            id,
            member: new.member,
            label: new.label,
            credential_id: new.credential_id,
            credential_json: new.credential_json,
            created_at: new.created_at,
            last_used_at: None,
        })
    }

    /// Store the updated credential state after a successful authentication.
    ///
    /// # Errors
    ///
    /// `NotFound` if the passkey does not exist.
    pub async fn update_passkey_after_use(
        &mut self,
        id: PasskeyId,
        credential_json: String,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result =
            sqlx::query("UPDATE passkeys SET credential_json = ?, last_used_at = ? WHERE id = ?")
                .bind(credential_json)
                .bind(at.as_millisecond())
                .bind(id.to_string())
                .execute(&mut *self.tx)
                .await
                .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Delete a passkey belonging to `member`.
    ///
    /// # Errors
    ///
    /// `NotFound` if the passkey does not exist or belongs to someone else.
    pub async fn delete_passkey(
        &mut self,
        member: MemberId,
        id: PasskeyId,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("DELETE FROM passkeys WHERE id = ? AND member_id = ?")
            .bind(id.to_string())
            .bind(member.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Fetch one of `member`'s passkeys, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if the passkey does not exist or belongs to someone else.
    pub async fn get_passkey(
        &mut self,
        member: MemberId,
        id: PasskeyId,
    ) -> Result<PasskeyRecord, StoreError> {
        let row = sqlx::query(
            "SELECT id, member_id, label, credential_id, credential_json, created_at, \
             last_used_at FROM passkeys WHERE id = ? AND member_id = ?",
        )
        .bind(id.to_string())
        .bind(member.to_string())
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        passkey_from_row(&row)
    }

    /// Number of passkeys a member has, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn count_passkeys(&mut self, member: MemberId) -> Result<u32, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM passkeys WHERE member_id = ?")
            .bind(member.to_string())
            .fetch_one(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        u32::try_from(count).map_err(StoreError::backend)
    }
}

impl Store {
    /// A member's passkeys, oldest first.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_passkeys(&self, member: MemberId) -> Result<Vec<PasskeyRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, member_id, label, credential_id, credential_json, created_at, \
             last_used_at FROM passkeys WHERE member_id = ? ORDER BY id",
        )
        .bind(member.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(passkey_from_row).collect()
    }
}
