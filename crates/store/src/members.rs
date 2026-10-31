//! Household members.

use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::MemberId;
use jiff::Timestamp;
use sqlx::SqliteExecutor;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, text, time, time_opt};
use crate::{Store, StoreError, StoreTx};

/// The one definition of "an active admin", shared by every admin query.
macro_rules! active_admin {
    () => {
        "role = 'admin' AND disabled_at IS NULL"
    };
}

macro_rules! columns {
    () => {
        "id, username, display_name, role, created_at, disabled_at"
    };
}

/// A stored member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    pub username: Username,
    pub display_name: DisplayName,
    pub role: Role,
    pub created_at: Timestamp,
    pub disabled_at: Option<Timestamp>,
}

/// A member to create. The id is chosen by the caller, because a passkey
/// ceremony binds it as the `WebAuthn` user handle before the member exists.
#[derive(Debug, Clone)]
pub struct NewMember {
    pub id: MemberId,
    pub username: Username,
    pub display_name: DisplayName,
    pub role: Role,
    pub created_at: Timestamp,
}

pub(crate) fn member_from_row(row: &SqliteRow) -> Result<Member, StoreError> {
    Ok(Member {
        id: parsed(row, "id")?,
        username: Username::parse(&text(row, "username")?).map_err(StoreError::backend)?,
        display_name: DisplayName::parse(&text(row, "display_name")?)
            .map_err(StoreError::backend)?,
        role: parsed(row, "role")?,
        created_at: time(row, "created_at")?,
        disabled_at: time_opt(row, "disabled_at")?,
    })
}

impl StoreTx {
    /// Create a member.
    ///
    /// # Errors
    ///
    /// `Conflict("username taken")` if the username exists. Callers pass a
    /// fresh [`MemberId::new`], so an id collision is not a practical case.
    pub async fn insert_member(&mut self, new: NewMember) -> Result<Member, StoreError> {
        let id = new.id;
        sqlx::query(
            "INSERT INTO members (id, username, display_name, role, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(new.username.as_str())
        .bind(new.display_name.as_str())
        .bind(new.role.as_str())
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(|err| match StoreError::from_sqlx(err) {
            StoreError::Conflict(_) => StoreError::Conflict("username taken"),
            other => other,
        })?;
        Ok(Member {
            id,
            username: new.username,
            display_name: new.display_name,
            role: new.role,
            created_at: new.created_at,
            disabled_at: None,
        })
    }

    /// Change a member's role.
    ///
    /// # Errors
    ///
    /// `NotFound` if the member does not exist.
    pub async fn set_member_role(&mut self, id: MemberId, role: Role) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE members SET role = ? WHERE id = ?")
            .bind(role.as_str())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Mark a member disabled at `at`.
    ///
    /// # Errors
    ///
    /// `NotFound` if the member does not exist.
    pub async fn set_member_disabled(
        &mut self,
        id: MemberId,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE members SET disabled_at = ? WHERE id = ?")
            .bind(at.as_millisecond())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Fetch a member by id, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such member.
    pub async fn get_member(&mut self, id: MemberId) -> Result<Member, StoreError> {
        fetch_member(&mut *self.tx, id).await
    }

    /// Number of enabled admins, as seen by this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn count_active_admins(&mut self) -> Result<u32, StoreError> {
        let count: i64 = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM members WHERE ",
            active_admin!()
        ))
        .fetch_one(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        u32::try_from(count).map_err(StoreError::backend)
    }
}

async fn fetch_member<'e>(
    executor: impl SqliteExecutor<'e>,
    id: MemberId,
) -> Result<Member, StoreError> {
    let row = sqlx::query(concat!("SELECT ", columns!(), " FROM members WHERE id = ?"))
        .bind(id.to_string())
        .fetch_one(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    member_from_row(&row)
}

pub(crate) fn expect_one(rows_affected: u64) -> Result<(), StoreError> {
    if rows_affected == 0 {
        Err(StoreError::NotFound)
    } else {
        Ok(())
    }
}

impl Store {
    /// Fetch a member by id.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such member.
    pub async fn get_member(&self, id: MemberId) -> Result<Member, StoreError> {
        fetch_member(&self.pool, id).await
    }

    /// Fetch a member by username.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such member.
    pub async fn get_member_by_username(&self, username: &Username) -> Result<Member, StoreError> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM members WHERE username = ?"
        ))
        .bind(username.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        member_from_row(&row)
    }

    /// All members, oldest first.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_members(&self) -> Result<Vec<Member>, StoreError> {
        let rows = sqlx::query(concat!("SELECT ", columns!(), " FROM members ORDER BY id"))
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        rows.iter().map(member_from_row).collect()
    }

    /// Whether any enabled admin exists.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn any_admin_exists(&self) -> Result<bool, StoreError> {
        sqlx::query_scalar(concat!(
            "SELECT EXISTS (SELECT 1 FROM members WHERE ",
            active_admin!(),
            ")"
        ))
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)
    }
}
