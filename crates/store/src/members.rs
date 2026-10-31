//! Household members.

use coffer_domain::identity::{ColorScheme, DisplayName, Role, ThemeName, Username};
use coffer_domain::ids::MemberId;
use jiff::Timestamp;
use sqlx::SqliteExecutor;
use sqlx::sqlite::SqliteRow;

use crate::convert::{parsed, text, text_opt, time, time_opt};
use crate::{Store, StoreError, StoreTx};

/// The one definition of "an active admin", shared by every admin query.
macro_rules! active_admin {
    () => {
        "role = 'admin' AND disabled_at IS NULL"
    };
}

macro_rules! columns {
    () => {
        "id, username, display_name, role, created_at, disabled_at, theme, color_scheme"
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
    /// The member's chosen theme; `None` follows the household theme.
    pub theme: Option<ThemeName>,
    pub color_scheme: ColorScheme,
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

/// Decode a stored theme name. A value [`ThemeName::parse`] rejects reads as
/// `None` ("no theme") instead of failing: the schema's CHECK makes it
/// unreachable unless constraint checks were disabled, and failing would turn
/// every lookup of that member, sign-in included, into an error. This crate
/// has no logging, so nothing is reported; the value is simply ignored.
pub(crate) fn theme_from_row(
    row: &SqliteRow,
    column: &str,
) -> Result<Option<ThemeName>, StoreError> {
    Ok(text_opt(row, column)?.and_then(|name| ThemeName::parse(&name).ok()))
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
        theme: theme_from_row(row, "theme")?,
        color_scheme: parsed(row, "color_scheme")?,
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
            theme: None,
            color_scheme: ColorScheme::System,
        })
    }

    /// Set a member's theme (`None` follows the household theme) and colour
    /// scheme.
    ///
    /// # Errors
    ///
    /// `NotFound` if the member does not exist.
    pub async fn set_member_appearance(
        &mut self,
        id: MemberId,
        theme: Option<&ThemeName>,
        scheme: ColorScheme,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE members SET theme = ?, color_scheme = ? WHERE id = ?")
            .bind(theme.map(ThemeName::as_str))
            .bind(scheme.as_str())
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
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

#[cfg(test)]
mod tests {
    use coffer_domain::identity::ColorScheme;

    use super::*;

    async fn store_with_member() -> (tempfile::TempDir, Store, MemberId) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        let member = tx
            .insert_member(NewMember {
                id: MemberId::new(),
                username: Username::parse("alice").unwrap(),
                display_name: DisplayName::parse("Alice").unwrap(),
                role: Role::Member,
                created_at: Timestamp::from_millisecond(0).unwrap(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (dir, store, member.id)
    }

    #[tokio::test]
    async fn invalid_theme_values_are_refused_by_the_schema() {
        let (_dir, store, member) = store_with_member().await;
        let too_long = "a".repeat(33);
        for bad in ["Mochi", "a b", "", too_long.as_str()] {
            let result = sqlx::query("UPDATE members SET theme = ? WHERE id = ?")
                .bind(bad)
                .bind(member.to_string())
                .execute(&store.pool)
                .await;
            assert!(result.is_err(), "members.theme accepted {bad:?}");
        }
        let result = sqlx::query("UPDATE members SET color_scheme = 'purple' WHERE id = ?")
            .bind(member.to_string())
            .execute(&store.pool)
            .await;
        assert!(result.is_err(), "members.color_scheme accepted purple");
        for bad in ["X", "a b", "", too_long.as_str()] {
            let result = sqlx::query("INSERT INTO household_appearance (id, theme) VALUES (1, ?)")
                .bind(bad)
                .execute(&store.pool)
                .await;
            assert!(
                result.is_err(),
                "household_appearance.theme accepted {bad:?}"
            );
        }
        let result = sqlx::query("INSERT INTO household_appearance (id, theme) VALUES (2, 'a')")
            .execute(&store.pool)
            .await;
        assert!(result.is_err(), "a second household_appearance row");

        for good in ["mochi", "a-1"] {
            sqlx::query("UPDATE members SET theme = ? WHERE id = ?")
                .bind(good)
                .bind(member.to_string())
                .execute(&store.pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO household_appearance (id, theme) VALUES (1, ?) \
                 ON CONFLICT (id) DO UPDATE SET theme = excluded.theme",
            )
            .bind(good)
            .execute(&store.pool)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn unparseable_stored_theme_reads_as_none() {
        let (_dir, store, member) = store_with_member().await;
        let mut tx = store.begin().await.unwrap();
        tx.set_member_appearance(member, None, ColorScheme::Dark)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        store
            .execute_unchecked_for_tests(
                "UPDATE members SET theme = 'Mochi' WHERE id = ?",
                &member.to_string(),
            )
            .await
            .unwrap();
        store
            .execute_unchecked_for_tests(
                "INSERT INTO household_appearance (id, theme) VALUES (1, ?)",
                "Mochi",
            )
            .await
            .unwrap();

        let read = store.get_member(member).await.unwrap();
        assert_eq!(read.theme, None);
        assert_eq!(read.color_scheme, ColorScheme::Dark);
        let listed = store.list_members().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].theme, None);
        assert_eq!(store.household_theme().await.unwrap(), None);
        let mut tx = store.begin().await.unwrap();
        assert_eq!(tx.get_member(member).await.unwrap().theme, None);
        assert_eq!(tx.household_theme().await.unwrap(), None);
        drop(tx);

        // The hook turns the checks back on when it is done.
        let result = sqlx::query("UPDATE members SET theme = 'Bad' WHERE id = ?")
            .bind(member.to_string())
            .execute(&store.pool)
            .await;
        assert!(result.is_err());
    }
}
