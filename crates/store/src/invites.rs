//! Bootstrap, new-member and re-enroll invites.

use coffer_domain::identity::{DisplayName, Role};
use coffer_domain::ids::{InviteId, MemberId};
use jiff::Timestamp;
use sqlx::SqliteExecutor;
use sqlx::sqlite::SqliteRow;

use crate::convert::{corrupt, parsed, parsed_opt, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, kind, role, display_name, target_member_id, created_by, created_at, \
                       expires_at, used_at, used_by, revoked_at"
    };
}

/// What redeeming an invite does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteKind {
    /// Creates the first admin.
    Bootstrap,
    /// Creates a new member.
    NewMember {
        role: Role,
        display_name: Option<DisplayName>,
    },
    /// Adds a passkey to an existing member.
    Reenroll { member: MemberId },
}

/// A stored invite. The token hash is never read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub id: InviteId,
    pub kind: InviteKind,
    pub created_by: Option<MemberId>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub used_at: Option<Timestamp>,
    pub used_by: Option<MemberId>,
    pub revoked_at: Option<Timestamp>,
}

/// An invite to create.
#[derive(Debug, Clone)]
pub struct NewInvite {
    pub token_hash: [u8; 32],
    pub kind: InviteKind,
    pub created_by: Option<MemberId>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

fn invite_from_row(row: &SqliteRow) -> Result<Invite, StoreError> {
    let kind = match text(row, "kind")?.as_str() {
        "bootstrap" => InviteKind::Bootstrap,
        "new_member" => InviteKind::NewMember {
            role: parsed(row, "role")?,
            display_name: text_opt(row, "display_name")?
                .map(|name| DisplayName::parse(&name))
                .transpose()
                .map_err(StoreError::backend)?,
        },
        "reenroll" => InviteKind::Reenroll {
            member: parsed_opt(row, "target_member_id")?
                .ok_or_else(|| corrupt("reenroll invite without target member"))?,
        },
        _ => return Err(corrupt("unknown invite kind")),
    };
    Ok(Invite {
        id: parsed(row, "id")?,
        kind,
        created_by: parsed_opt(row, "created_by")?,
        created_at: time(row, "created_at")?,
        expires_at: time(row, "expires_at")?,
        used_at: time_opt(row, "used_at")?,
        used_by: parsed_opt(row, "used_by")?,
        revoked_at: time_opt(row, "revoked_at")?,
    })
}

impl StoreTx {
    /// Create an invite.
    ///
    /// # Errors
    ///
    /// `Conflict` if the token hash already exists.
    pub async fn insert_invite(&mut self, new: NewInvite) -> Result<Invite, StoreError> {
        let id = InviteId::new();
        let (kind, role, display_name, target) = match &new.kind {
            InviteKind::Bootstrap => ("bootstrap", None, None, None),
            InviteKind::NewMember { role, display_name } => (
                "new_member",
                Some(role.as_str()),
                display_name.as_ref().map(DisplayName::as_str),
                None,
            ),
            InviteKind::Reenroll { member } => ("reenroll", None, None, Some(member.to_string())),
        };
        sqlx::query(
            "INSERT INTO invites \
             (id, token_hash, kind, role, display_name, target_member_id, created_by, \
              created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.token_hash.as_slice())
        .bind(kind)
        .bind(role)
        .bind(display_name)
        .bind(target)
        .bind(new.created_by.map(|m| m.to_string()))
        .bind(new.created_at.as_millisecond())
        .bind(new.expires_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(Invite {
            id,
            kind: new.kind,
            created_by: new.created_by,
            created_at: new.created_at,
            expires_at: new.expires_at,
            used_at: None,
            used_by: None,
            revoked_at: None,
        })
    }

    /// Mark an invite used. A single conditional update, so two concurrent
    /// redemptions cannot both succeed.
    ///
    /// # Errors
    ///
    /// `Conflict("invite already used or expired")` if the invite is unknown,
    /// used, revoked, or `now >= expires_at`.
    pub async fn consume_invite(
        &mut self,
        id: InviteId,
        used_by: MemberId,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE invites SET used_at = ?, used_by = ? \
             WHERE id = ? AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?",
        )
        .bind(now.as_millisecond())
        .bind(used_by.to_string())
        .bind(id.to_string())
        .bind(now.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        if result.rows_affected() == 0 {
            Err(StoreError::Conflict("invite already used or expired"))
        } else {
            Ok(())
        }
    }

    /// Find an invite that can still be redeemed, as seen by this
    /// transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if unknown, used, revoked, or `now >= expires_at`.
    pub async fn find_usable_invite(
        &mut self,
        token_hash: &[u8; 32],
        now: Timestamp,
    ) -> Result<Invite, StoreError> {
        fetch_usable_invite(&mut *self.tx, token_hash, now).await
    }

    /// Invites that are unused, unrevoked and unexpired at `now`, as seen by
    /// this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_pending_invites(
        &mut self,
        now: Timestamp,
    ) -> Result<Vec<Invite>, StoreError> {
        fetch_pending_invites(&mut *self.tx, now).await
    }

    /// Revoke an invite.
    ///
    /// # Errors
    ///
    /// `NotFound` if the invite does not exist.
    pub async fn revoke_invite(&mut self, id: InviteId, at: Timestamp) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE invites SET revoked_at = ? WHERE id = ?")
            .bind(at.as_millisecond())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }
}

impl Store {
    /// Find an invite that can still be redeemed.
    ///
    /// # Errors
    ///
    /// `NotFound` if unknown, used, revoked, or `now >= expires_at`.
    pub async fn find_usable_invite(
        &self,
        token_hash: &[u8; 32],
        now: Timestamp,
    ) -> Result<Invite, StoreError> {
        fetch_usable_invite(&self.pool, token_hash, now).await
    }

    /// Invites that are unused, unrevoked and unexpired at `now`.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_pending_invites(&self, now: Timestamp) -> Result<Vec<Invite>, StoreError> {
        fetch_pending_invites(&self.pool, now).await
    }
}

async fn fetch_usable_invite<'e>(
    executor: impl SqliteExecutor<'e>,
    token_hash: &[u8; 32],
    now: Timestamp,
) -> Result<Invite, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM invites WHERE token_hash = ? \
         AND used_at IS NULL AND revoked_at IS NULL AND expires_at > ?"
    ))
    .bind(token_hash.as_slice())
    .bind(now.as_millisecond())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    invite_from_row(&row)
}

async fn fetch_pending_invites<'e>(
    executor: impl SqliteExecutor<'e>,
    now: Timestamp,
) -> Result<Vec<Invite>, StoreError> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM invites \
         WHERE used_at IS NULL AND revoked_at IS NULL AND expires_at > ? ORDER BY id"
    ))
    .bind(now.as_millisecond())
    .fetch_all(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    rows.iter().map(invite_from_row).collect()
}
