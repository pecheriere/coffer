//! The household's forecast settings (spec §6.8, §8.9).

use serde::{Deserialize, Serialize};

use crate::money::Money;
use crate::plan::{
    MAX_FLOOR_MONTHS, MAX_HORIZON_MONTHS, MAX_PLAN_AMOUNT_MINOR, MAX_STALE_AFTER_DAYS,
    MIN_HORIZON_MONTHS, PlanError,
};

/// The cash that must stay untouched on top of the horizon's planned spending
/// and goals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SafetyFloor {
    /// A fixed amount, 0 included, in its own currency.
    Fixed { amount: Money },
    /// This many months of planned recurring outflows, per currency: 1 to 60.
    MonthsOfOutflows {
        #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 60))]
        months: u8,
    },
}

impl SafetyFloor {
    /// Checks the floor: a fixed amount from 0 to [`MAX_PLAN_AMOUNT_MINOR`],
    /// or 1 to 60 months.
    ///
    /// # Errors
    /// Returns [`PlanError::InvalidFloor`] or [`PlanError::InvalidFloorMonths`].
    pub fn validate(&self) -> Result<(), PlanError> {
        match *self {
            Self::Fixed { amount }
                if !(0..=MAX_PLAN_AMOUNT_MINOR).contains(&amount.minor_units) =>
            {
                Err(PlanError::InvalidFloor)
            }
            Self::MonthsOfOutflows { months } if !(1..=MAX_FLOOR_MONTHS).contains(&months) => {
                Err(PlanError::InvalidFloorMonths)
            }
            Self::Fixed { .. } | Self::MonthsOfOutflows { .. } => Ok(()),
        }
    }

    /// The floor as text for the audit log: `fixed:USD:100000` or
    /// `months_of_outflows:3`.
    #[must_use]
    pub fn canonical(&self) -> String {
        match self {
            Self::Fixed { amount } => format!("fixed:{}:{}", amount.currency, amount.minor_units),
            Self::MonthsOfOutflows { months } => format!("months_of_outflows:{months}"),
        }
    }
}

/// The household's defaults for a forecast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ForecastSettings {
    /// The safety floor. Until one is set a forecast has no number: a silent
    /// floor of 0 would overstate what can be invested.
    pub floor: Option<SafetyFloor>,
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
            floor: None,
            include_income: false,
            default_horizon_months: 3,
            stale_after_days: 7,
        }
    }
}

impl ForecastSettings {
    /// Checks the floor, the horizon and the staleness threshold.
    ///
    /// # Errors
    /// Returns the first [`PlanError`] found.
    pub fn validate(&self) -> Result<(), PlanError> {
        if let Some(floor) = &self.floor {
            floor.validate()?;
        }
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
    use super::{ForecastSettings, SafetyFloor};
    use crate::money::{Currency, Money};
    use crate::plan::{MAX_PLAN_AMOUNT_MINOR, PlanError};

    fn fixed(minor_units: i64) -> SafetyFloor {
        SafetyFloor::Fixed {
            amount: Money {
                minor_units,
                currency: Currency::parse("USD").unwrap(),
            },
        }
    }

    #[test]
    fn defaults_have_no_floor() {
        let settings = ForecastSettings::default();
        assert_eq!(settings.floor, None, "no floor until one is set");
        assert!(!settings.include_income, "income is off by default");
        assert_eq!(settings.default_horizon_months, 3, "three months");
        assert_eq!(settings.stale_after_days, 7, "a week");
        assert_eq!(settings.validate(), Ok(()), "the defaults are valid");
    }

    #[test]
    fn zero_fixed_floor_is_valid() {
        assert_eq!(
            fixed(0).validate(),
            Ok(()),
            "an explicit floor of 0 is allowed"
        );
        assert_eq!(
            fixed(MAX_PLAN_AMOUNT_MINOR).validate(),
            Ok(()),
            "the largest floor"
        );
    }

    #[test]
    fn floor_validation_refuses_out_of_range() {
        for bad in [-1, MAX_PLAN_AMOUNT_MINOR + 1, i64::MIN, i64::MAX] {
            assert_eq!(
                fixed(bad).validate(),
                Err(PlanError::InvalidFloor),
                "fixed floor range"
            );
        }
        for bad in [0, 61, u8::MAX] {
            assert_eq!(
                SafetyFloor::MonthsOfOutflows { months: bad }.validate(),
                Err(PlanError::InvalidFloorMonths),
                "floor months range"
            );
        }
        for good in [1, 60] {
            assert_eq!(
                SafetyFloor::MonthsOfOutflows { months: good }.validate(),
                Ok(()),
                "floor months bounds"
            );
        }
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
        let settings = ForecastSettings {
            floor: Some(fixed(-1)),
            ..base
        };
        assert_eq!(
            settings.validate(),
            Err(PlanError::InvalidFloor),
            "the floor is checked too"
        );
        let settings = ForecastSettings {
            floor: Some(SafetyFloor::MonthsOfOutflows { months: 0 }),
            ..base
        };
        assert_eq!(
            settings.validate(),
            Err(PlanError::InvalidFloorMonths),
            "the floor months are checked too"
        );
    }

    #[test]
    fn floor_canonical_text() {
        assert_eq!(
            fixed(100_000).canonical(),
            "fixed:USD:100000",
            "fixed floor"
        );
        assert_eq!(fixed(0).canonical(), "fixed:USD:0", "zero floor");
        assert_eq!(
            SafetyFloor::MonthsOfOutflows { months: 3 }.canonical(),
            "months_of_outflows:3",
            "months floor"
        );
    }

    #[test]
    fn floor_serde_is_tagged() {
        let text = r#"{"kind":"months_of_outflows","months":3}"#;
        let floor = SafetyFloor::MonthsOfOutflows { months: 3 };
        assert_eq!(serde_json::to_string(&floor).unwrap(), text, "months shape");
        assert_eq!(
            serde_json::from_str::<SafetyFloor>(text).unwrap(),
            floor,
            "months roundtrip"
        );
        let text = r#"{"kind":"fixed","amount":{"minor_units":100,"currency":"USD"}}"#;
        assert_eq!(
            serde_json::from_str::<SafetyFloor>(text).unwrap(),
            fixed(100),
            "fixed shape"
        );
    }

    #[test]
    fn unknown_floor_and_settings_fields_are_refused() {
        for text in [
            r#"{"kind":"months_of_outflows","months":3,"amount":1}"#,
            r#"{"kind":"percent","months":3}"#,
            r#"{"kind":"months_of_outflows","months":3.5}"#,
            r#"{"kind":"fixed","amount":{"minor_units":1.5,"currency":"USD"}}"#,
        ] {
            assert!(
                serde_json::from_str::<SafetyFloor>(text).is_err(),
                "a bad floor is refused"
            );
        }
        let text = r#"{"floor":null,"include_income":false,"default_horizon_months":3,"stale_after_days":7,"timezone":"UTC"}"#;
        assert!(
            serde_json::from_str::<ForecastSettings>(text).is_err(),
            "an unknown settings field is refused"
        );
    }
}
