//! Agent API tokens. Only the hash of the token is stored, and it is never
//! read back.

use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::{AgentTokenId, MemberId};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, text, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, member_id, name, scope, prefix, created_at, expires_at, last_used_at, revoked_at"
    };
}

/// A stored agent token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTokenRecord {
    pub id: AgentTokenId,
    pub member: MemberId,
    pub name: TokenName,
    pub scope: Scope,
    pub prefix: String,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}

/// An agent token to create.
#[derive(Debug, Clone)]
pub struct NewAgentToken {
    pub member: MemberId,
    pub name: TokenName,
    pub scope: Scope,
    pub token_hash: [u8; 32],
    pub prefix: String,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
}

fn token_from_row(row: &SqliteRow) -> Result<AgentTokenRecord, StoreError> {
    Ok(AgentTokenRecord {
        id: parsed(row, "id")?,
        member: parsed(row, "member_id")?,
        name: TokenName::parse(&text(row, "name")?).map_err(StoreError::backend)?,
        scope: parsed(row, "scope")?,
        prefix: text(row, "prefix")?,
        created_at: time(row, "created_at")?,
        expires_at: time_opt(row, "expires_at")?,
        last_used_at: time_opt(row, "last_used_at")?,
        revoked_at: time_opt(row, "revoked_at")?,
    })
}

impl StoreTx {
    /// Create an agent token.
    ///
    /// # Errors
    ///
    /// `Conflict` if the token hash already exists.
    pub async fn insert_agent_token(
        &mut self,
        new: NewAgentToken,
    ) -> Result<AgentTokenRecord, StoreError> {
        let id = AgentTokenId::new();
        sqlx::query(
            "INSERT INTO agent_tokens \
             (id, member_id, name, scope, token_hash, prefix, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.member.to_string())
        .bind(new.name.as_str())
        .bind(new.scope.as_str())
        .bind(new.token_hash.as_slice())
        .bind(&new.prefix)
        .bind(new.created_at.as_millisecond())
        .bind(new.expires_at.map(Timestamp::as_millisecond))
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(AgentTokenRecord {
            id,
            member: new.member,
            name: new.name,
            scope: new.scope,
            prefix: new.prefix,
            created_at: new.created_at,
            expires_at: new.expires_at,
            last_used_at: None,
            revoked_at: None,
        })
    }

    /// Revoke one token.
    ///
    /// # Errors
    ///
    /// `NotFound` if the token does not exist.
    pub async fn revoke_agent_token(
        &mut self,
        id: AgentTokenId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE agent_tokens SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?",
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Revoke every not-yet-revoked token of a member (zero is fine).
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn revoke_tokens_for_member(
        &mut self,
        member: MemberId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE agent_tokens SET revoked_at = ? WHERE member_id = ? AND revoked_at IS NULL",
        )
        .bind(at.as_millisecond())
        .bind(member.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// Record that a live (unrevoked) token was just used.
    ///
    /// # Errors
    ///
    /// `NotFound` if the token does not exist or has been revoked.
    pub async fn touch_agent_token(
        &mut self,
        id: AgentTokenId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE agent_tokens SET last_used_at = ? WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(at.as_millisecond())
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }
}

impl Store {
    /// Fetch one token by id, revoked or expired included. Never includes
    /// the hash.
    ///
    /// # Errors
    ///
    /// `NotFound` if the token does not exist.
    pub async fn get_agent_token(&self, id: AgentTokenId) -> Result<AgentTokenRecord, StoreError> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_tokens WHERE id = ?"
        ))
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        token_from_row(&row)
    }

    /// Find a token that is neither revoked nor expired.
    ///
    /// # Errors
    ///
    /// `NotFound` if unknown, revoked, or `now >= expires_at`.
    pub async fn find_live_agent_token(
        &self,
        token_hash: &[u8; 32],
        now: Timestamp,
    ) -> Result<AgentTokenRecord, StoreError> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_tokens WHERE token_hash = ? AND revoked_at IS NULL \
             AND (expires_at IS NULL OR expires_at > ?)"
        ))
        .bind(token_hash.as_slice())
        .bind(now.as_millisecond())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        token_from_row(&row)
    }

    /// Tokens of one member, or of everyone when `member` is `None`; newest
    /// last. Never includes hashes.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_agent_tokens(
        &self,
        member: Option<MemberId>,
    ) -> Result<Vec<AgentTokenRecord>, StoreError> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_tokens \
             WHERE (?1 IS NULL OR member_id = ?1) ORDER BY id"
        ))
        .bind(member.map(|m| m.to_string()))
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(token_from_row).collect()
    }
}
