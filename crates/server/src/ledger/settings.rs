//! The household's forecast settings (spec §6.8, §8.9).

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::plan::{ForecastSettings, PlanError, SafetyFloor};
use coffer_domain::policy::{Action, authorize};
use serde::Deserialize;
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit};

/// The audit entity of every settings change.
const ENTITY: &str = "household";

/// The settings to change. A field left out stays as it is. A floor can be
/// replaced but never unset: `null` leaves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ForecastSettingsPatch {
    /// A new safety floor; `null` or absent keeps the current one.
    #[serde(default)]
    pub floor: Option<SafetyFloor>,
    #[serde(default)]
    pub include_income: Option<bool>,
    /// 1 to 60 months.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 60)]
    pub default_horizon_months: Option<u8>,
    /// 1 to 365 days.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 365)]
    pub stale_after_days: Option<u16>,
}

/// The message for a rejected setting; never echoes the input.
fn invalid(error: PlanError) -> LedgerError {
    LedgerError::Invalid(match error {
        PlanError::InvalidFloor => "a fixed floor must be between 0 and 10^15 minor units",
        PlanError::InvalidFloorMonths => "floor months must be 1 to 60",
        PlanError::InvalidHorizon => "the horizon must be 1 to 60 months",
        PlanError::InvalidStaleDays => "stale after must be 1 to 365 days",
        _ => "the forecast settings are not valid",
    })
}

impl Ledger {
    /// The household's forecast settings: the defaults until some are set.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage
    /// failures.
    pub async fn forecast_settings(&self, actor: &Actor) -> Result<ForecastSettings, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        Ok(self.store.forecast_settings().await?)
    }

    /// Changes the forecast settings. A patch that changes nothing writes
    /// nothing, audit included.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ManageHousehold`];
    /// [`LedgerError::Invalid`] for a value out of range; storage failures.
    pub async fn update_forecast_settings(
        &self,
        actor: &Actor,
        patch: ForecastSettingsPatch,
    ) -> Result<ForecastSettings, LedgerError> {
        authorize(actor, Action::ManageHousehold)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.forecast_settings().await?;
        let next = ForecastSettings {
            floor: patch.floor.or(current.floor),
            include_income: patch.include_income.unwrap_or(current.include_income),
            default_horizon_months: patch
                .default_horizon_months
                .unwrap_or(current.default_horizon_months),
            stale_after_days: patch.stale_after_days.unwrap_or(current.stale_after_days),
        };
        next.validate().map_err(invalid)?;

        let mut changes = AuditChanges::new();
        if next.floor != current.floor {
            changes = changes.change(
                "floor",
                current.floor.map(|floor| floor.canonical()),
                next.floor.map(|floor| floor.canonical()),
            );
        }
        if next.include_income != current.include_income {
            changes = changes.change(
                "include_income",
                current.include_income,
                next.include_income,
            );
        }
        if next.default_horizon_months != current.default_horizon_months {
            changes = changes.change(
                "default_horizon_months",
                i64::from(current.default_horizon_months),
                i64::from(next.default_horizon_months),
            );
        }
        if next.stale_after_days != current.stale_after_days {
            changes = changes.change(
                "stale_after_days",
                i64::from(current.stale_after_days),
                i64::from(next.stale_after_days),
            );
        }
        if changes.is_empty() {
            return Ok(current);
        }
        tx.set_forecast_settings(&next).await?;
        audit(
            &mut tx,
            now,
            actor,
            "household.settings",
            ENTITY,
            ENTITY,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(next)
    }
}
