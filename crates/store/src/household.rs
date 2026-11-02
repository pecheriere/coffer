//! Household-wide settings.

use coffer_domain::identity::ThemeName;
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{ForecastSettings, SafetyFloor};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteExecutor};

use crate::convert::{corrupt, text_opt};
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

fn small<T: TryFrom<i64>>(row: &SqliteRow, column: &str) -> Result<T, StoreError> {
    let value: i64 = row.try_get(column).map_err(StoreError::from_sqlx)?;
    T::try_from(value).map_err(|_| corrupt("setting out of range"))
}

fn floor_from_row(row: &SqliteRow) -> Result<Option<SafetyFloor>, StoreError> {
    let kind = text_opt(row, "floor_kind")?;
    let minor: Option<i64> = row.try_get("floor_minor").map_err(StoreError::from_sqlx)?;
    let currency = text_opt(row, "floor_currency")?;
    let months: Option<i64> = row.try_get("floor_months").map_err(StoreError::from_sqlx)?;
    match (kind.as_deref(), minor, currency, months) {
        (None, None, None, None) => Ok(None),
        (Some("fixed"), Some(minor_units), Some(currency), None) => Ok(Some(SafetyFloor::Fixed {
            amount: Money {
                minor_units,
                currency: Currency::parse(&currency).map_err(StoreError::backend)?,
            },
        })),
        (Some("months_of_outflows"), None, None, Some(months)) => {
            Ok(Some(SafetyFloor::MonthsOfOutflows {
                months: u8::try_from(months).map_err(|_| corrupt("floor months out of range"))?,
            }))
        }
        _ => Err(corrupt("floor columns do not form a floor")),
    }
}

fn settings_from_row(row: &SqliteRow) -> Result<ForecastSettings, StoreError> {
    let settings = ForecastSettings {
        floor: floor_from_row(row)?,
        include_income: match small::<u8>(row, "include_income")? {
            0 => false,
            1 => true,
            _ => return Err(corrupt("flag is neither 0 nor 1")),
        },
        default_horizon_months: small(row, "default_horizon_months")?,
        stale_after_days: small(row, "stale_after_days")?,
    };
    settings
        .validate()
        .map_err(|_| corrupt("stored forecast settings are invalid"))?;
    Ok(settings)
}

async fn fetch_forecast_settings<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<ForecastSettings, StoreError> {
    sqlx::query("SELECT * FROM household_settings WHERE id = 1")
        .fetch_optional(executor)
        .await
        .map_err(StoreError::from_sqlx)?
        .map_or_else(
            || Ok(ForecastSettings::default()),
            |row| settings_from_row(&row),
        )
}

impl Store {
    /// The household's forecast settings: the defaults while none were set.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not valid settings.
    pub async fn forecast_settings(&self) -> Result<ForecastSettings, StoreError> {
        fetch_forecast_settings(&self.pool).await
    }

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
    /// The forecast settings as seen by this transaction; see
    /// [`Store::forecast_settings`].
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not valid settings.
    pub async fn forecast_settings(&mut self) -> Result<ForecastSettings, StoreError> {
        fetch_forecast_settings(&mut *self.tx).await
    }

    /// Replace the forecast settings. The caller validates them; the
    /// schema's CHECKs refuse what is out of range.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn set_forecast_settings(
        &mut self,
        settings: &ForecastSettings,
    ) -> Result<(), StoreError> {
        let (kind, minor, currency, months) = match settings.floor {
            None => (None, None, None, None),
            Some(SafetyFloor::Fixed { amount }) => (
                Some("fixed"),
                Some(amount.minor_units),
                Some(amount.currency.to_string()),
                None,
            ),
            Some(SafetyFloor::MonthsOfOutflows { months }) => (
                Some("months_of_outflows"),
                None,
                None,
                Some(i64::from(months)),
            ),
        };
        sqlx::query(
            "INSERT INTO household_settings (id, floor_kind, floor_minor, floor_currency, \
             floor_months, include_income, default_horizon_months, stale_after_days) \
             VALUES (1, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET floor_kind = excluded.floor_kind, \
             floor_minor = excluded.floor_minor, floor_currency = excluded.floor_currency, \
             floor_months = excluded.floor_months, include_income = excluded.include_income, \
             default_horizon_months = excluded.default_horizon_months, \
             stale_after_days = excluded.stale_after_days",
        )
        .bind(kind)
        .bind(minor)
        .bind(currency)
        .bind(months)
        .bind(i64::from(settings.include_income))
        .bind(i64::from(settings.default_horizon_months))
        .bind(i64::from(settings.stale_after_days))
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

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
