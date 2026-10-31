//! Account classification and validated account and connection names.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::identity::label_type;

/// Why an account value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("account name must be 1 to 100 characters without control characters")]
    InvalidName,
    #[error("connection label must be 1 to 100 characters without control characters")]
    InvalidConnectionLabel,
    #[error("unknown account kind")]
    UnknownKind,
    #[error("unknown liquidity class")]
    UnknownLiquidity,
}

/// What an account is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Checking,
    Savings,
    CreditCard,
    Loan,
    Mortgage,
    Brokerage,
    Retirement,
    EquityComp,
    Property,
    Vehicle,
    OtherAsset,
    OtherLiability,
}

impl AccountKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Savings => "savings",
            Self::CreditCard => "credit_card",
            Self::Loan => "loan",
            Self::Mortgage => "mortgage",
            Self::Brokerage => "brokerage",
            Self::Retirement => "retirement",
            Self::EquityComp => "equity_comp",
            Self::Property => "property",
            Self::Vehicle => "vehicle",
            Self::OtherAsset => "other_asset",
            Self::OtherLiability => "other_liability",
        }
    }
}

impl FromStr for AccountKind {
    type Err = AccountError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "checking" => Self::Checking,
            "savings" => Self::Savings,
            "credit_card" => Self::CreditCard,
            "loan" => Self::Loan,
            "mortgage" => Self::Mortgage,
            "brokerage" => Self::Brokerage,
            "retirement" => Self::Retirement,
            "equity_comp" => Self::EquityComp,
            "property" => Self::Property,
            "vehicle" => Self::Vehicle,
            "other_asset" => Self::OtherAsset,
            "other_liability" => Self::OtherLiability,
            _ => return Err(AccountError::UnknownKind),
        })
    }
}

/// How quickly an account's balance can be spent, or must be repaid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum LiquidityClass {
    Cash,
    ShortTermLiability,
    Invested,
    Illiquid,
    LongTermLiability,
}

impl LiquidityClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::ShortTermLiability => "short_term_liability",
            Self::Invested => "invested",
            Self::Illiquid => "illiquid",
            Self::LongTermLiability => "long_term_liability",
        }
    }
}

impl FromStr for LiquidityClass {
    type Err = AccountError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "cash" => Self::Cash,
            "short_term_liability" => Self::ShortTermLiability,
            "invested" => Self::Invested,
            "illiquid" => Self::Illiquid,
            "long_term_liability" => Self::LongTermLiability,
            _ => return Err(AccountError::UnknownLiquidity),
        })
    }
}

/// The liquidity class an account of `kind` gets unless one is chosen.
#[must_use]
pub fn default_liquidity(kind: AccountKind) -> LiquidityClass {
    match kind {
        AccountKind::Checking | AccountKind::Savings => LiquidityClass::Cash,
        AccountKind::CreditCard => LiquidityClass::ShortTermLiability,
        AccountKind::Brokerage | AccountKind::Retirement | AccountKind::EquityComp => {
            LiquidityClass::Invested
        }
        AccountKind::Property | AccountKind::Vehicle | AccountKind::OtherAsset => {
            LiquidityClass::Illiquid
        }
        AccountKind::Loan | AccountKind::Mortgage | AccountKind::OtherLiability => {
            LiquidityClass::LongTermLiability
        }
    }
}

label_type!(
    /// An account's human-readable name.
    AccountName,
    AccountError::InvalidName,
    max = 100
);
label_type!(
    /// A human-readable label for a provider connection.
    ConnectionLabel,
    AccountError::InvalidConnectionLabel,
    max = 100
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{DisplayName, IdentityError};

    const ALL_KINDS: [AccountKind; 12] = [
        AccountKind::Checking,
        AccountKind::Savings,
        AccountKind::CreditCard,
        AccountKind::Loan,
        AccountKind::Mortgage,
        AccountKind::Brokerage,
        AccountKind::Retirement,
        AccountKind::EquityComp,
        AccountKind::Property,
        AccountKind::Vehicle,
        AccountKind::OtherAsset,
        AccountKind::OtherLiability,
    ];

    #[test]
    fn default_liquidity_table() {
        use AccountKind as K;
        use LiquidityClass as L;
        for (kind, class) in [
            (K::Checking, L::Cash),
            (K::Savings, L::Cash),
            (K::CreditCard, L::ShortTermLiability),
            (K::Brokerage, L::Invested),
            (K::Retirement, L::Invested),
            (K::EquityComp, L::Invested),
            (K::Property, L::Illiquid),
            (K::Vehicle, L::Illiquid),
            (K::OtherAsset, L::Illiquid),
            (K::Loan, L::LongTermLiability),
            (K::Mortgage, L::LongTermLiability),
            (K::OtherLiability, L::LongTermLiability),
        ] {
            assert_eq!(default_liquidity(kind), class, "{kind:?}");
        }
    }

    #[test]
    fn kinds_and_classes_roundtrip_as_snake_case() {
        for kind in ALL_KINDS {
            assert_eq!(kind.as_str().parse::<AccountKind>(), Ok(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert_eq!(AccountKind::CreditCard.as_str(), "credit_card");
        assert_eq!(AccountKind::EquityComp.as_str(), "equity_comp");
        assert_eq!(
            "CreditCard".parse::<AccountKind>(),
            Err(AccountError::UnknownKind)
        );
        for class in [
            LiquidityClass::Cash,
            LiquidityClass::ShortTermLiability,
            LiquidityClass::Invested,
            LiquidityClass::Illiquid,
            LiquidityClass::LongTermLiability,
        ] {
            assert_eq!(class.as_str().parse::<LiquidityClass>(), Ok(class));
            assert_eq!(
                serde_json::to_string(&class).unwrap(),
                format!("\"{}\"", class.as_str())
            );
        }
        assert_eq!(
            LiquidityClass::ShortTermLiability.as_str(),
            "short_term_liability"
        );
        assert_eq!(
            "liquid".parse::<LiquidityClass>(),
            Err(AccountError::UnknownLiquidity)
        );
    }

    #[test]
    fn labels_respect_their_maximum() {
        let hundred = "é".repeat(100);
        assert_eq!(AccountName::parse(&hundred).unwrap().as_str(), hundred);
        assert_eq!(
            AccountName::parse(&"é".repeat(101)),
            Err(AccountError::InvalidName)
        );
        assert_eq!(
            ConnectionLabel::parse(&"x".repeat(100))
                .unwrap()
                .as_str()
                .len(),
            100
        );
        assert_eq!(
            ConnectionLabel::parse(&"x".repeat(101)),
            Err(AccountError::InvalidConnectionLabel)
        );
        assert!(DisplayName::parse(&"x".repeat(64)).is_ok());
        assert_eq!(
            DisplayName::parse(&"x".repeat(65)),
            Err(IdentityError::InvalidDisplayName)
        );
        assert_eq!(
            AccountName::parse("  Joint checking ").unwrap().as_str(),
            "Joint checking"
        );
        for bad in ["", "   ", "a\u{0007}b"] {
            assert_eq!(
                AccountName::parse(bad),
                Err(AccountError::InvalidName),
                "{bad:?}"
            );
        }
    }
}
