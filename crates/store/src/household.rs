//! Household-wide settings.

use coffer_domain::identity::ThemeName;
use sqlx::SqliteExecutor;

use crate::members::theme_from_row;
use crate::{Store, StoreError, StoreTx};

async fn fetch_household_theme<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<Option<ThemeName>, StoreError> {
    let row = sqlx::query("SELECT theme FROM household_appearance WHERE id = 1")
        .fetch_optional(executor)
        .await
        .map_err(StoreError::from_sqlx)?;
    match row {
        Some(row) => theme_from_row(&row, "theme"),
        None => Ok(None),
    }
}

impl Store {
    /// The household theme, or `None` if none was chosen. A stored value
    /// that is not a valid theme name reads as `None`, like a member's.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn household_theme(&self) -> Result<Option<ThemeName>, StoreError> {
        fetch_household_theme(&self.pool).await
    }
}

impl StoreTx {
    /// The household theme as seen by this transaction; see
    /// [`Store::household_theme`].
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn household_theme(&mut self) -> Result<Option<ThemeName>, StoreError> {
        fetch_household_theme(&mut *self.tx).await
    }

    /// Set the household theme.
    ///
    /// # Errors
    ///
    /// Backend failures only.
    pub async fn set_household_theme(&mut self, theme: &ThemeName) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO household_appearance (id, theme) VALUES (1, ?) \
             ON CONFLICT (id) DO UPDATE SET theme = excluded.theme",
        )
        .bind(theme.as_str())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}
