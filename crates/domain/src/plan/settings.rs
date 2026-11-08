//! The household's forecast settings (spec §6.8, §8.9).

use serde::{Deserialize, Serialize};

use crate::plan::{MAX_HORIZON_MONTHS, MAX_STALE_AFTER_DAYS, MIN_HORIZON_MONTHS, PlanError};

/// The household's defaults for a forecast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ForecastSettings {
    /// Whether planned inflows reduce the cash needed.
    pub include_income: bool,
    /// The horizon of a forecast that names none: 1 to 60 months.
    #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 60))]
    pub default_horizon_months: u8,
    /// How old a balance may be before it is flagged: 1 to 365 days.
    #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 365))]
    pub stale_after_days: u16,
}

impl Default for ForecastSettings {
    fn default() -> Self {
        Self {
            include_income: false,
            default_horizon_months: 3,
            stale_after_days: 7,
        }
    }
}

impl ForecastSettings {
    /// Checks the horizon and the staleness threshold.
    ///
    /// # Errors
    /// Returns the first [`PlanError`] found.
    pub fn validate(&self) -> Result<(), PlanError> {
        if !(MIN_HORIZON_MONTHS..=MAX_HORIZON_MONTHS).contains(&self.default_horizon_months) {
            return Err(PlanError::InvalidHorizon);
        }
        if !(1..=MAX_STALE_AFTER_DAYS).contains(&self.stale_after_days) {
            return Err(PlanError::InvalidStaleDays);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ForecastSettings;
    use crate::plan::PlanError;

    #[test]
    fn defaults() {
        let settings = ForecastSettings::default();
        assert!(!settings.include_income, "income is off by default");
        assert_eq!(settings.default_horizon_months, 3, "three months");
        assert_eq!(settings.stale_after_days, 7, "a week");
        assert_eq!(settings.validate(), Ok(()), "the defaults are valid");
    }

    #[test]
    fn settings_validation_refuses_out_of_range() {
        let base = ForecastSettings::default();
        for bad in [0, 61] {
            let settings = ForecastSettings {
                default_horizon_months: bad,
                ..base
            };
            assert_eq!(
                settings.validate(),
                Err(PlanError::InvalidHorizon),
                "horizon range"
            );
        }
        for good in [1, 60] {
            let settings = ForecastSettings {
                default_horizon_months: good,
                ..base
            };
            assert_eq!(settings.validate(), Ok(()), "horizon bounds");
        }
        for bad in [0, 366] {
            let settings = ForecastSettings {
                stale_after_days: bad,
                ..base
            };
            assert_eq!(
                settings.validate(),
                Err(PlanError::InvalidStaleDays),
                "staleness range"
            );
        }
        for good in [1, 365] {
            let settings = ForecastSettings {
                stale_after_days: good,
                ..base
            };
            assert_eq!(settings.validate(), Ok(()), "staleness bounds");
        }
    }

    #[test]
    fn unknown_settings_fields_are_refused() {
        let text = r#"{"include_income":false,"default_horizon_months":3,"stale_after_days":7,"timezone":"UTC"}"#;
        assert!(
            serde_json::from_str::<ForecastSettings>(text).is_err(),
            "an unknown settings field is refused"
        );
    }
}
