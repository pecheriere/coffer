//! Transactions, their categorized splits and the categories they use:
//! review status, origin, flags, the split rule and validated texts.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::identity::label_type;
use crate::ids::CategoryId;

/// The most splits one transaction may have.
pub const MAX_SPLITS: usize = 50;

/// The longest note, in characters.
pub const MAX_NOTES_CHARS: usize = 2000;

/// The highest position a category group or a category may take. Groups
/// and categories are ordered by position, then by age.
pub const MAX_POSITION: u32 = 10_000;

/// Why a transaction or category value was rejected. Messages never echo
/// the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransactionError {
    #[error("payee must be 1 to 200 characters without control characters")]
    InvalidPayee,
    #[error("description must be 1 to 500 characters without control characters")]
    InvalidDescription,
    #[error("memo must be 1 to 500 characters without control characters")]
    InvalidMemo,
    #[error("notes must be at most 2000 characters without control characters other than newlines")]
    InvalidNotes,
    #[error("category name must be 1 to 100 characters without control characters")]
    InvalidCategoryName,
    #[error("category group name must be 1 to 100 characters without control characters")]
    InvalidCategoryGroupName,
    #[error("unknown review status")]
    UnknownReviewStatus,
    #[error("unknown category kind")]
    UnknownCategoryKind,
    #[error("unknown transaction origin")]
    UnknownOrigin,
}

/// Why a set of splits does not fit its transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SplitError {
    #[error("a transaction has at most 50 splits")]
    TooMany,
    #[error("a split amount cannot be zero")]
    ZeroSplit,
    #[error("the splits must sum exactly to the transaction amount")]
    SumMismatch,
}

/// Defines a string enum: `as_str`, `FromStr` (rejecting with the given
/// error) and snake-case serde.
macro_rules! string_enum {
    (
        $(#[$meta:meta])* $name:ident, $error:ident::$variant:ident,
        { $($(#[$vmeta:meta])* $item:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $item),+
        }

        impl $name {
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$item => $text),+
                }
            }
        }

        impl FromStr for $name {
            type Err = $error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok(Self::$item),)+
                    _ => Err($error::$variant),
                }
            }
        }
    };
}

string_enum!(
    /// Whether a member (or an agent) has looked at a transaction.
    ReviewStatus, TransactionError::UnknownReviewStatus,
    {
        Unreviewed => "unreviewed",
        Reviewed => "reviewed",
    }
);

string_enum!(
    /// What a category counts: spending, income, or money moving between
    /// the household's own accounts.
    CategoryKind, TransactionError::UnknownCategoryKind,
    {
        Expense => "expense",
        Income => "income",
        Transfer => "transfer",
    }
);

string_enum!(
    /// How a transaction came to exist.
    TransactionOrigin, TransactionError::UnknownOrigin,
    {
        /// Entered by a member or an agent on a manual account; never
        /// touched by a sync.
        Manual => "manual",
        /// Created from a provider's records by a sync.
        Imported => "imported",
    }
);

/// Something about a transaction that needs a person's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum TransactionFlag {
    /// It may be the same purchase as another transaction.
    PossibleDuplicate,
    /// The provider no longer reports it, and it holds user changes, so it
    /// was kept.
    SourceRemoved,
    /// Its imported amount changed and its several splits no longer sum to
    /// it.
    SplitMismatch,
}

impl TransactionFlag {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PossibleDuplicate => "possible_duplicate",
            Self::SourceRemoved => "source_removed",
            Self::SplitMismatch => "split_mismatch",
        }
    }
}

/// One categorized part of a transaction's amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SplitInput {
    pub category: CategoryId,
    /// This part in the transaction currency's minor units, with the
    /// transaction's sign convention; never zero.
    pub minor_units: i64,
}

/// Checks that `splits` fit a transaction of `amount_minor`: none at all
/// (uncategorized), or 1 to [`MAX_SPLITS`] non-zero splits summing exactly
/// to the amount.
///
/// # Errors
///
/// [`SplitError::TooMany`], [`SplitError::ZeroSplit`] or
/// [`SplitError::SumMismatch`].
pub fn validate_splits(amount_minor: i64, splits: &[SplitInput]) -> Result<(), SplitError> {
    if splits.len() > MAX_SPLITS {
        return Err(SplitError::TooMany);
    }
    if splits.iter().any(|split| split.minor_units == 0) {
        return Err(SplitError::ZeroSplit);
    }
    // At most 50 `i64` values: their sum always fits an `i128`.
    let sum: i128 = splits
        .iter()
        .map(|split| i128::from(split.minor_units))
        .sum();
    if splits.is_empty() || sum == i128::from(amount_minor) {
        Ok(())
    } else {
        Err(SplitError::SumMismatch)
    }
}

/// Which fields of a transaction a member or an agent has set. A sync never
/// discards a transaction holding any of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent yes/no per user field, read together by the sync"
)]
pub struct UserState {
    pub splits: bool,
    pub notes: bool,
    pub reviewed: bool,
    pub assignee: bool,
    pub payee_edited: bool,
}

impl UserState {
    /// Whether no user field is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !(self.splits || self.notes || self.reviewed || self.assignee || self.payee_edited)
    }
}

label_type!(
    /// Who a transaction was with, as shown to members.
    Payee,
    TransactionError::InvalidPayee,
    max = 200
);
label_type!(
    /// A transaction's description, as its source gave it.
    Description,
    TransactionError::InvalidDescription,
    max = 500
);
label_type!(
    /// A transaction's memo, as its source gave it.
    Memo,
    TransactionError::InvalidMemo,
    max = 500
);
label_type!(
    /// A category's name.
    CategoryName,
    TransactionError::InvalidCategoryName,
    max = 100
);
label_type!(
    /// A category group's name.
    CategoryGroupName,
    TransactionError::InvalidCategoryGroupName,
    max = 100
);

/// A member's notes on a transaction: at most [`MAX_NOTES_CHARS`]
/// characters, kept exactly as written; newlines are the only control
/// characters allowed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Notes(String);

impl Notes {
    /// Validates `input` without trimming it.
    ///
    /// # Errors
    ///
    /// [`TransactionError::InvalidNotes`] when `input` is too long or holds
    /// a control character other than `\n`.
    pub fn parse(input: &str) -> Result<Self, TransactionError> {
        let valid = input.chars().count() <= MAX_NOTES_CHARS
            && !input.chars().any(|c| c.is_control() && c != '\n');
        if valid {
            Ok(Self(input.to_owned()))
        } else {
            Err(TransactionError::InvalidNotes)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the notes are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::Strategy as _;

    use crate::ids::CategoryId;

    fn split(minor_units: i64) -> SplitInput {
        SplitInput {
            category: CategoryId::new(),
            minor_units,
        }
    }

    #[test]
    fn splits_must_sum_to_amount() {
        assert_eq!(validate_splits(-1_000, &[]), Ok(()), "uncategorized");
        assert_eq!(validate_splits(-1_000, &[split(-600), split(-400)]), Ok(()));
        assert_eq!(
            validate_splits(-1_000, &[split(-600), split(-401)]),
            Err(SplitError::SumMismatch)
        );
        assert_eq!(
            validate_splits(-1_000, &[split(-600), split(-399)]),
            Err(SplitError::SumMismatch)
        );
        let fifty: Vec<_> = (0..50).map(|_| split(-20)).collect();
        assert_eq!(validate_splits(-1_000, &fifty), Ok(()));
        let fifty_one: Vec<_> = (0..51).map(|_| split(-20)).collect();
        assert_eq!(
            validate_splits(-1_020, &fifty_one),
            Err(SplitError::TooMany)
        );
        assert_eq!(
            validate_splits(-1_000, &[split(-1_000), split(0)]),
            Err(SplitError::ZeroSplit)
        );
        assert_eq!(
            validate_splits(0, &[split(500), split(-500)]),
            Ok(()),
            "a zero amount may still split into opposite parts"
        );
        assert_eq!(
            validate_splits(i64::MIN, &[split(i64::MIN), split(-1), split(1)]),
            Ok(()),
            "sums never overflow"
        );
        assert_eq!(
            validate_splits(i64::MAX, &[split(i64::MAX), split(i64::MAX)]),
            Err(SplitError::SumMismatch)
        );
    }

    proptest::proptest! {
        #[test]
        fn any_partition_of_the_amount_validates(
            parts in proptest::collection::vec(
                (-1_000_000_000_i64..1_000_000_000).prop_filter("no zero split", |p| *p != 0),
                1..=50,
            ),
        ) {
            let amount: i64 = parts.iter().sum();
            let splits: Vec<_> = parts.iter().map(|p| split(*p)).collect();
            proptest::prop_assert_eq!(validate_splits(amount, &splits), Ok(()));
            proptest::prop_assert_eq!(
                validate_splits(amount + 1, &splits),
                Err(SplitError::SumMismatch)
            );
        }
    }

    #[test]
    fn user_state_is_empty_only_without_any_user_field() {
        assert!(UserState::default().is_empty());
        for state in [
            UserState {
                splits: true,
                ..UserState::default()
            },
            UserState {
                notes: true,
                ..UserState::default()
            },
            UserState {
                reviewed: true,
                ..UserState::default()
            },
            UserState {
                assignee: true,
                ..UserState::default()
            },
            UserState {
                payee_edited: true,
                ..UserState::default()
            },
        ] {
            assert!(!state.is_empty(), "{state:?}");
        }
    }

    #[test]
    fn text_limits() {
        assert_eq!(
            Payee::parse(" Corner shop ").unwrap().as_str(),
            "Corner shop"
        );
        assert!(Payee::parse(&"x".repeat(200)).is_ok());
        assert_eq!(
            Payee::parse(&"x".repeat(201)),
            Err(TransactionError::InvalidPayee)
        );
        assert_eq!(Payee::parse(""), Err(TransactionError::InvalidPayee));
        assert_eq!(Payee::parse("a\nb"), Err(TransactionError::InvalidPayee));

        assert!(Description::parse(&"é".repeat(500)).is_ok());
        assert_eq!(
            Description::parse(&"é".repeat(501)),
            Err(TransactionError::InvalidDescription)
        );
        assert_eq!(
            Description::parse("  "),
            Err(TransactionError::InvalidDescription)
        );

        assert_eq!(Notes::parse("").unwrap().as_str(), "");
        assert_eq!(
            Notes::parse("line one\nline two").unwrap().as_str(),
            "line one\nline two"
        );
        assert!(Notes::parse(&"é".repeat(2000)).is_ok());
        assert_eq!(
            Notes::parse(&"é".repeat(2001)),
            Err(TransactionError::InvalidNotes)
        );
        for bad in ["a\rb", "a\tb", "a\u{0007}b", "a\u{0085}b"] {
            assert_eq!(
                Notes::parse(bad),
                Err(TransactionError::InvalidNotes),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn enums_roundtrip_as_snake_case() {
        for status in [ReviewStatus::Unreviewed, ReviewStatus::Reviewed] {
            assert_eq!(status.as_str().parse::<ReviewStatus>(), Ok(status));
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{}\"", status.as_str())
            );
        }
        for kind in [
            CategoryKind::Expense,
            CategoryKind::Income,
            CategoryKind::Transfer,
        ] {
            assert_eq!(kind.as_str().parse::<CategoryKind>(), Ok(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        for origin in [TransactionOrigin::Manual, TransactionOrigin::Imported] {
            assert_eq!(origin.as_str().parse::<TransactionOrigin>(), Ok(origin));
        }
        for flag in [
            TransactionFlag::PossibleDuplicate,
            TransactionFlag::SourceRemoved,
            TransactionFlag::SplitMismatch,
        ] {
            assert_eq!(
                serde_json::to_string(&flag).unwrap(),
                format!("\"{}\"", flag.as_str())
            );
        }
        assert_eq!(
            TransactionFlag::PossibleDuplicate.as_str(),
            "possible_duplicate"
        );
        assert_eq!(
            "Reviewed".parse::<ReviewStatus>(),
            Err(TransactionError::UnknownReviewStatus)
        );
        assert_eq!(
            "cost".parse::<CategoryKind>(),
            Err(TransactionError::UnknownCategoryKind)
        );
        assert_eq!(
            "sync".parse::<TransactionOrigin>(),
            Err(TransactionError::UnknownOrigin)
        );
    }
}
