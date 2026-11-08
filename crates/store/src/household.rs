//! Household-wide settings.

use coffer_domain::identity::ThemeName;
use coffer_domain::money::Currency;
use coffer_domain::plan::ForecastSettings;
use coffer_domain::time::IanaZone;
use jiff::civil::Date;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteExecutor};

use crate::convert::corrupt;
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

fn settings_from_row(row: &SqliteRow) -> Result<ForecastSettings, StoreError> {
    let settings = ForecastSettings {
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

pub(crate) async fn fetch_forecast_settings<'e>(
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

pub(crate) async fn fetch_display_currency<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<Option<Currency>, StoreError> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT display_currency FROM household_settings WHERE id = 1")
            .fetch_optional(executor)
            .await
            .map_err(StoreError::from_sqlx)?;
    stored
        .flatten()
        .map(|code| Currency::parse(&code).map_err(StoreError::backend))
        .transpose()
}

pub(crate) async fn fetch_household_zone<'e>(
    executor: impl SqliteExecutor<'e>,
) -> Result<Option<IanaZone>, StoreError> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT timezone FROM household_settings WHERE id = 1")
            .fetch_optional(executor)
            .await
            .map_err(StoreError::from_sqlx)?;
    stored
        .flatten()
        .map(|name| IanaZone::parse(&name).map_err(StoreError::backend))
        .transpose()
}

/// Every household setting, seen at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdSettingsRecord {
    pub forecast: ForecastSettings,
    /// `None` while none was chosen.
    pub display_currency: Option<Currency>,
    /// `None` while none was chosen, and the server's default applies.
    pub timezone: Option<IanaZone>,
}

impl Store {
    /// The zone an admin chose for the household, or `None` while the server's
    /// default applies.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored name that is not a zone.
    pub async fn household_zone(&self) -> Result<Option<IanaZone>, StoreError> {
        fetch_household_zone(&self.pool).await
    }

    /// The currency the household's net worth is shown in, or `None` while
    /// none was chosen.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored code that is not a currency.
    pub async fn display_currency(&self) -> Result<Option<Currency>, StoreError> {
        fetch_display_currency(&self.pool).await
    }

    /// The household's forecast settings: the defaults while none were set.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not valid settings.
    pub async fn forecast_settings(&self) -> Result<ForecastSettings, StoreError> {
        fetch_forecast_settings(&self.pool).await
    }

    /// Every household setting, seen at one moment: read apart, a patch changing
    /// several could show between them as a state that never existed.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that is not valid settings, a stored
    /// code that is not a currency or a stored name that is not a zone.
    pub async fn household_settings(&self) -> Result<HouseholdSettingsRecord, StoreError> {
        let mut tx = self.pool.begin().await.map_err(StoreError::from_sqlx)?;
        let forecast = fetch_forecast_settings(&mut *tx).await?;
        let display_currency = fetch_display_currency(&mut *tx).await?;
        let timezone = fetch_household_zone(&mut *tx).await?;
        tx.commit().await.map_err(StoreError::from_sqlx)?;
        Ok(HouseholdSettingsRecord {
            forecast,
            display_currency,
            timezone,
        })
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
        sqlx::query(
            "INSERT INTO household_settings (id, include_income, default_horizon_months, \
             stale_after_days) VALUES (1, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET include_income = excluded.include_income, \
             default_horizon_months = excluded.default_horizon_months, \
             stale_after_days = excluded.stale_after_days",
        )
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

    /// The display currency as seen by this transaction; see
    /// [`Store::display_currency`].
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored code that is not a currency.
    pub async fn display_currency(&mut self) -> Result<Option<Currency>, StoreError> {
        fetch_display_currency(&mut *self.tx).await
    }

    /// With no settings row yet, makes it with the forecast defaults; an existing
    /// row keeps every value.
    async fn ensure_settings_row(&mut self) -> Result<(), StoreError> {
        let defaults = ForecastSettings::default();
        sqlx::query(
            "INSERT INTO household_settings (id, include_income, default_horizon_months, \
             stale_after_days) VALUES (1, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
        )
        .bind(i64::from(defaults.include_income))
        .bind(i64::from(defaults.default_horizon_months))
        .bind(i64::from(defaults.stale_after_days))
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(())
    }

    /// The zone chosen for the household as seen by this transaction; see
    /// [`Store::household_zone`].
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored name that is not a zone.
    pub async fn household_zone(&mut self) -> Result<Option<IanaZone>, StoreError> {
        fetch_household_zone(&mut *self.tx).await
    }

    /// Choose the household's zone, or with `None` return to the server's default.
    /// `today` is the household's day in the zone that now applies. Nothing already
    /// stored moves: a day stays the day it was given. Only a daily table that now
    /// runs past `today` changes, since it would hold a day still to come.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored row that no longer parses.
    pub async fn set_household_zone(
        &mut self,
        zone: Option<&IanaZone>,
        today: Date,
    ) -> Result<(), StoreError> {
        self.ensure_settings_row().await?;
        sqlx::query("UPDATE household_settings SET timezone = ? WHERE id = 1")
            .bind(zone.map(IanaZone::name))
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        self.refresh_after_zone_change(today).await
    }

    /// Set the display currency and answer the one it replaced, which is the
    /// new one again when nothing changed. With no settings row yet, the row
    /// is made with the forecast defaults; an existing row keeps every other
    /// value. A change rewrites every daily row in this transaction.
    ///
    /// # Errors
    ///
    /// Backend failures.
    pub async fn set_display_currency(
        &mut self,
        currency: Currency,
    ) -> Result<Option<Currency>, StoreError> {
        let previous = fetch_display_currency(&mut *self.tx).await?;
        self.ensure_settings_row().await?;
        sqlx::query("UPDATE household_settings SET display_currency = ? WHERE id = 1")
            .bind(currency.as_str())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        if previous != Some(currency) {
            self.refresh_after_display_change().await?;
        }
        Ok(previous)
    }
}
