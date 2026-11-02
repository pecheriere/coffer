//! Account classification and validated account and connection names.

pub mod belonging;

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory as _};

use crate::identity::{label_type, parse_label};
use crate::transactions::string_enum;

/// Why an account value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.message())]
pub enum AccountError {
    InvalidName,
    InvalidConnectionLabel,
    UnknownKind,
    UnknownLiquidity,
    UnknownBalanceSource,
    UnknownBelongsTo,
    InvalidBeneficiary,
    CountedButNotTheHouseholds,
    BeneficiaryOnTheHousehold,
}

impl AccountError {
    /// The refusal's fixed words, which outlive the error.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidName => {
                "account name must be 1 to 100 characters without control characters"
            }
            Self::InvalidConnectionLabel => {
                "connection label must be 1 to 100 characters without control characters"
            }
            Self::UnknownKind => "unknown account kind",
            Self::UnknownLiquidity => "unknown liquidity class",
            Self::UnknownBalanceSource => "unknown balance source",
            Self::UnknownBelongsTo => "unknown value for whose money an account is",
            Self::InvalidBeneficiary => {
                "beneficiary must be 1 to 64 characters with something visible and no control, \
                 format or separator characters"
            }
            Self::CountedButNotTheHouseholds => {
                "an account that isn't the household's can't be counted: set belongs_to to \
                 household first"
            }
            Self::BeneficiaryOnTheHousehold => {
                "only an account that isn't the household's has a beneficiary"
            }
        }
    }
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
    /// A 529 and similar.
    Education,
    /// UTMA, UGMA.
    Custodial,
    /// A donor-advised fund.
    Charitable,
    EquityComp,
    Property,
    Vehicle,
    OtherAsset,
    OtherLiability,
}

impl AccountKind {
    /// Every kind, in the order a form lists them.
    pub const ALL: [Self; 15] = [
        Self::Checking,
        Self::Savings,
        Self::CreditCard,
        Self::Loan,
        Self::Mortgage,
        Self::Brokerage,
        Self::Retirement,
        Self::Education,
        Self::Custodial,
        Self::Charitable,
        Self::EquityComp,
        Self::Property,
        Self::Vehicle,
        Self::OtherAsset,
        Self::OtherLiability,
    ];

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
            Self::Education => "education",
            Self::Custodial => "custodial",
            Self::Charitable => "charitable",
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
            "education" => Self::Education,
            "custodial" => Self::Custodial,
            "charitable" => Self::Charitable,
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

/// Where a balance snapshot came from. A day holds one snapshot, whichever
/// was written last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum BalanceSource {
    /// Reported by a provider sync.
    Sync,
    /// Entered by a member or an agent.
    Manual,
}

impl BalanceSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Manual => "manual",
        }
    }
}

impl FromStr for BalanceSource {
    type Err = AccountError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sync" => Ok(Self::Sync),
            "manual" => Ok(Self::Manual),
            _ => Err(AccountError::UnknownBalanceSource),
        }
    }
}

string_enum!(
    /// Whose money an account holds, apart from what kind of account it is. It says how
    /// coffer treats the money and is not a legal statement.
    BelongsTo, AccountError::UnknownBelongsTo,
    {
        /// The household's own money.
        Household => "household",
        /// Held for someone, or for a purpose beyond the household's own.
        SomeoneElse => "someone_else",
        /// No longer the household's, whatever has happened to it since.
        GivenAway => "given_away",
    }
);

/// The liquidity class an account of `kind` gets unless one is chosen.
#[must_use]
pub fn default_liquidity(kind: AccountKind) -> LiquidityClass {
    match kind {
        AccountKind::Checking | AccountKind::Savings => LiquidityClass::Cash,
        AccountKind::CreditCard => LiquidityClass::ShortTermLiability,
        AccountKind::Brokerage
        | AccountKind::Retirement
        | AccountKind::Education
        | AccountKind::Custodial
        | AccountKind::Charitable
        | AccountKind::EquityComp => LiquidityClass::Invested,
        AccountKind::Property | AccountKind::Vehicle | AccountKind::OtherAsset => {
            LiquidityClass::Illiquid
        }
        AccountKind::Loan | AccountKind::Mortgage | AccountKind::OtherLiability => {
            LiquidityClass::LongTermLiability
        }
    }
}

/// Whose money an account of `kind` holds unless that is chosen.
#[must_use]
pub fn default_belongs_to(kind: AccountKind) -> BelongsTo {
    match kind {
        AccountKind::Education | AccountKind::Custodial => BelongsTo::SomeoneElse,
        AccountKind::Charitable => BelongsTo::GivenAway,
        AccountKind::Checking
        | AccountKind::Savings
        | AccountKind::CreditCard
        | AccountKind::Loan
        | AccountKind::Mortgage
        | AccountKind::Brokerage
        | AccountKind::Retirement
        | AccountKind::EquityComp
        | AccountKind::Property
        | AccountKind::Vehicle
        | AccountKind::OtherAsset
        | AccountKind::OtherLiability => BelongsTo::Household,
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
label_type!(
    /// Who or what an account that isn't the household's is held for: a label, not a person.
    /// It names a bucket and may be a child's name, so what it holds can be seen, and its
    /// `Debug` holds none of it.
    Beneficiary,
    AccountError::InvalidBeneficiary,
    max = 64,
    rule = " with something visible and no control, format or separator characters",
    check = parse_beneficiary
);

impl std::fmt::Debug for Beneficiary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Beneficiary(<redacted>)")
    }
}

/// A format character reorders or hides text, and a separator other than the plain space
/// makes a second spelling of a label that looks the same.
fn is_hidden_or_separator(c: char) -> bool {
    match c.general_category() {
        GeneralCategory::Format
        | GeneralCategory::LineSeparator
        | GeneralCategory::ParagraphSeparator
        | GeneralCategory::SpaceSeparator => c != ' ',
        _ => false,
    }
}

/// Whether `c` draws something by itself: a mark needs a base, an unassigned or private
/// character says nothing, and these letters and this symbol are blank.
fn is_visible(c: char) -> bool {
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter
            | GeneralCategoryGroup::Number
            | GeneralCategoryGroup::Punctuation
            | GeneralCategoryGroup::Symbol
    ) && !matches!(
        c,
        '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' | '\u{2800}'
    )
}

fn parse_beneficiary(input: &str, max: usize) -> Option<String> {
    parse_label(input, max)
        .filter(|label| !label.chars().any(is_hidden_or_separator) && label.chars().any(is_visible))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{DisplayName, IdentityError};

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
            (K::Education, L::Invested),
            (K::Custodial, L::Invested),
            (K::Charitable, L::Invested),
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
        for kind in AccountKind::ALL {
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
        for source in [BalanceSource::Sync, BalanceSource::Manual] {
            assert_eq!(source.as_str().parse::<BalanceSource>(), Ok(source));
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{}\"", source.as_str())
            );
        }
        assert_eq!(
            "Manual".parse::<BalanceSource>(),
            Err(AccountError::UnknownBalanceSource)
        );
        for belongs in [
            BelongsTo::Household,
            BelongsTo::SomeoneElse,
            BelongsTo::GivenAway,
        ] {
            assert_eq!(belongs.as_str().parse::<BelongsTo>(), Ok(belongs));
            assert_eq!(
                serde_json::to_string(&belongs).unwrap(),
                format!("\"{}\"", belongs.as_str())
            );
        }
        assert_eq!(BelongsTo::Household.as_str(), "household");
        assert_eq!(BelongsTo::SomeoneElse.as_str(), "someone_else");
        assert_eq!(BelongsTo::GivenAway.as_str(), "given_away");
        assert_eq!(
            "SomeoneElse".parse::<BelongsTo>(),
            Err(AccountError::UnknownBelongsTo)
        );
        assert_eq!(AccountKind::Education.as_str(), "education");
        assert_eq!(AccountKind::Custodial.as_str(), "custodial");
        assert_eq!(AccountKind::Charitable.as_str(), "charitable");
    }

    #[test]
    fn every_kind_is_listed_once() {
        let listed: std::collections::HashSet<_> = AccountKind::ALL.into_iter().collect();
        assert_eq!(listed.len(), AccountKind::ALL.len());
        let position = |kind| AccountKind::ALL.iter().position(|k| *k == kind);
        assert_eq!(
            [
                position(AccountKind::Retirement),
                position(AccountKind::Education),
                position(AccountKind::Custodial),
                position(AccountKind::Charitable),
                position(AccountKind::EquityComp),
            ],
            [Some(6), Some(7), Some(8), Some(9), Some(10)],
            "the three follow retirement"
        );
    }

    #[test]
    fn belongs_to_defaults_from_the_kind() {
        use AccountKind as K;
        use BelongsTo as B;
        for (kind, belongs) in [
            (K::Checking, B::Household),
            (K::Savings, B::Household),
            (K::CreditCard, B::Household),
            (K::Loan, B::Household),
            (K::Mortgage, B::Household),
            (K::Brokerage, B::Household),
            (K::Retirement, B::Household),
            (K::Education, B::SomeoneElse),
            (K::Custodial, B::SomeoneElse),
            (K::Charitable, B::GivenAway),
            (K::EquityComp, B::Household),
            (K::Property, B::Household),
            (K::Vehicle, B::Household),
            (K::OtherAsset, B::Household),
            (K::OtherLiability, B::Household),
        ] {
            assert_eq!(default_belongs_to(kind), belongs, "{kind:?}");
        }
    }

    #[test]
    fn beneficiary_respects_its_maximum() {
        let longest = "é".repeat(64);
        assert_eq!(Beneficiary::parse(&longest).unwrap().as_str(), longest);
        assert_eq!(
            Beneficiary::parse("  Child A ").unwrap().as_str(),
            "Child A"
        );
        for bad in ["", "   ", "Child\u{0007}A", &"é".repeat(65)] {
            let refused = Beneficiary::parse(bad);
            assert_eq!(refused, Err(AccountError::InvalidBeneficiary), "{bad:?}");
        }
        let message = Beneficiary::parse("Child\u{0007}A")
            .unwrap_err()
            .to_string();
        assert_eq!(
            message,
            "beneficiary must be 1 to 64 characters with something visible and no control, \
             format or separator characters"
        );
        assert!(
            !message.contains("Child"),
            "the error holds none of the input"
        );
    }

    #[test]
    fn beneficiary_refuses_what_can_t_be_seen() {
        for bad in [
            "Child\u{202E}A",   // right-to-left override
            "\u{202E}Child A",  // at the start, where trimming doesn't reach
            "Child\u{2028}A",   // line separator
            "Child\u{2029}A",   // paragraph separator
            "Child\u{200B}A",   // zero width space
            "Child\u{00AD}A",   // soft hyphen
            "Child\u{FEFF}A",   // byte order mark
            "Child\u{00A0}A",   // no-break space, a second spelling of a plain one
            "Child\u{3000}A",   // ideographic space
            "\u{200B}",         // nothing else
            "\u{200B}\u{200D}", // nothing else
            "\u{0301}",         // a mark with nothing to mark
            "\u{FE0F}",         // a variation selector with nothing to vary
            "\u{3164}",         // a blank letter
            "\u{FFA0}",         // a blank letter
            "\u{115F}\u{1160}", // blank letters
            "\u{2800}",         // a blank symbol
            "\u{E000}",         // private use: nothing says what it shows
        ] {
            assert_eq!(
                Beneficiary::parse(bad),
                Err(AccountError::InvalidBeneficiary),
                "{}",
                bad.escape_unicode()
            );
        }
    }

    #[test]
    fn beneficiary_keeps_what_people_write() {
        for good in [
            "Child A",
            "Family fund",
            "Zoe\u{0308}", // a mark on a letter
            "Zo\u{00EB} O'Neil-Smith",
            "\u{674E}\u{96F7}",                 // a name in Han
            "\u{0623}\u{062D}\u{0645}\u{062F}", // a name in Arabic
            "2030",
            "?",
            "\u{1F393}", // a symbol alone
        ] {
            assert_eq!(Beneficiary::parse(good).unwrap().as_str(), good);
        }
    }

    #[test]
    fn a_beneficiary_s_debug_is_redacted() {
        let label = Beneficiary::parse("Child A").unwrap();
        assert_eq!(format!("{label:?}"), "Beneficiary(<redacted>)");
        assert_eq!(label.to_string(), "Child A", "showing it stays deliberate");
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
