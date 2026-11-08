//! The emergency fund (money redesign §4.4, §10.4): how much cash to keep
//! untouched, one target per currency.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::money::Currency;
use crate::plan::{MAX_EMERGENCY_FUND_MONTHS, MAX_PLAN_AMOUNT_MINOR, PlanError};

/// How much cash to keep untouched in one currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmergencyFundTarget {
    /// A fixed amount in the currency's minor units, 0 to 10^15.
    Fixed {
        #[cfg_attr(
            feature = "openapi",
            schema(minimum = 0, maximum = 1_000_000_000_000_000_i64)
        )]
        minor_units: i64,
    },
    /// This many months, 1 to 60, of the currency's planned recurring outflows.
    MonthsOfOutflows {
        #[cfg_attr(feature = "openapi", schema(minimum = 1, maximum = 60))]
        months: u8,
    },
}

impl EmergencyFundTarget {
    /// Checks the target: a fixed amount from 0 to [`MAX_PLAN_AMOUNT_MINOR`],
    /// or 1 to 60 months.
    ///
    /// # Errors
    /// Returns [`PlanError::InvalidEmergencyFund`] or
    /// [`PlanError::InvalidEmergencyFundMonths`].
    pub fn validate(&self) -> Result<(), PlanError> {
        match *self {
            Self::Fixed { minor_units } if !(0..=MAX_PLAN_AMOUNT_MINOR).contains(&minor_units) => {
                Err(PlanError::InvalidEmergencyFund)
            }
            Self::MonthsOfOutflows { months }
                if !(1..=MAX_EMERGENCY_FUND_MONTHS).contains(&months) =>
            {
                Err(PlanError::InvalidEmergencyFundMonths)
            }
            Self::Fixed { .. } | Self::MonthsOfOutflows { .. } => Ok(()),
        }
    }

    /// The target as text for the audit log: `fixed:100000` or
    /// `months_of_outflows:3`.
    #[must_use]
    pub fn canonical(&self) -> String {
        match self {
            Self::Fixed { minor_units } => format!("fixed:{minor_units}"),
            Self::MonthsOfOutflows { months } => format!("months_of_outflows:{months}"),
        }
    }
}

/// One currency's target, as the API lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct EmergencyFund {
    /// The currency the target is kept in and applies to.
    pub currency: Currency,
    /// How much of it to keep untouched.
    pub target: EmergencyFundTarget,
}

/// Every currency's target; a currency missing has none.
pub type EmergencyFunds = BTreeMap<Currency, EmergencyFundTarget>;

#[cfg(test)]
mod tests {
    use super::EmergencyFundTarget;
    use crate::plan::{MAX_PLAN_AMOUNT_MINOR, PlanError};

    #[test]
    fn fixed_and_months_bounds() {
        for good in [0, MAX_PLAN_AMOUNT_MINOR] {
            assert_eq!(
                EmergencyFundTarget::Fixed { minor_units: good }.validate(),
                Ok(()),
                "a fixed target at its bounds"
            );
        }
        for bad in [-1, MAX_PLAN_AMOUNT_MINOR + 1, i64::MIN, i64::MAX] {
            assert_eq!(
                EmergencyFundTarget::Fixed { minor_units: bad }.validate(),
                Err(PlanError::InvalidEmergencyFund),
                "a fixed target out of range"
            );
        }
        for good in [1, 60] {
            assert_eq!(
                EmergencyFundTarget::MonthsOfOutflows { months: good }.validate(),
                Ok(()),
                "months at their bounds"
            );
        }
        for bad in [0, 61, u8::MAX] {
            assert_eq!(
                EmergencyFundTarget::MonthsOfOutflows { months: bad }.validate(),
                Err(PlanError::InvalidEmergencyFundMonths),
                "months out of range"
            );
        }
    }

    #[test]
    fn canonical_text() {
        assert_eq!(
            EmergencyFundTarget::Fixed {
                minor_units: 100_000
            }
            .canonical(),
            "fixed:100000",
            "a fixed target"
        );
        assert_eq!(
            EmergencyFundTarget::Fixed { minor_units: 0 }.canonical(),
            "fixed:0",
            "a zero target"
        );
        assert_eq!(
            EmergencyFundTarget::MonthsOfOutflows { months: 3 }.canonical(),
            "months_of_outflows:3",
            "a months target"
        );
    }

    #[test]
    fn serde_is_tagged_and_strict() {
        let text = r#"{"kind":"fixed","minor_units":100}"#;
        let target = EmergencyFundTarget::Fixed { minor_units: 100 };
        assert_eq!(serde_json::to_string(&target).unwrap(), text, "fixed shape");
        assert_eq!(
            serde_json::from_str::<EmergencyFundTarget>(text).unwrap(),
            target,
            "fixed roundtrip"
        );
        let text = r#"{"kind":"months_of_outflows","months":3}"#;
        assert_eq!(
            serde_json::from_str::<EmergencyFundTarget>(text).unwrap(),
            EmergencyFundTarget::MonthsOfOutflows { months: 3 },
            "months shape"
        );
        for bad in [
            r#"{"kind":"months_of_outflows","months":3,"minor_units":1}"#,
            r#"{"kind":"fixed","minor_units":1,"currency":"USD"}"#,
            r#"{"kind":"percent","months":3}"#,
            r#"{"kind":"months_of_outflows","months":3.5}"#,
            r#"{"kind":"fixed","minor_units":1.5}"#,
            r#"{"minor_units":1}"#,
        ] {
            assert!(
                serde_json::from_str::<EmergencyFundTarget>(bad).is_err(),
                "a bad target is refused: {bad}"
            );
        }
    }
}
