//! Rules: what files or assigns a transaction nobody has filled yet (spec §8.13).
//!
//! A rule matches on a transaction's description, read as plain text: `contains`
//! or `starts with`, ignoring case character by character. There are no regular
//! expressions, so neither a pattern nor a bank's text can make a match slow, and
//! both are only ever data.
//!
//! [`decide`] is the one place that says what rules do to a transaction. Rules are
//! read in order and, for each field, the first match wins. A rule only fills what
//! nobody has set, and never touches the review.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, CategoryId, MemberId, RuleId};
use crate::money::Currency;
use crate::transactions::{CategoryKind, string_enum};

/// The most rules a household may hold, which bounds the work a sync does for
/// each new transaction.
pub const MAX_RULES: usize = 200;

/// Why a rule's value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("the text to match must be 1 to 200 characters without control characters")]
    InvalidPattern,
    #[error("unknown match kind")]
    UnknownMatchKind,
    #[error("an amount range needs a lowest or a highest amount, lowest first")]
    InvalidAmountRange,
}

/// The text a rule looks for in a description, kept exactly as written: unlike a label it
/// isn't trimmed, since `"TAX "` is a way of not matching `TAXI`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RulePattern(String);

impl RulePattern {
    /// The most characters it may have.
    pub const MAX_CHARS: usize = 200;

    /// 1 to [`Self::MAX_CHARS`] characters, without control characters and not only spaces.
    ///
    /// # Errors
    ///
    /// [`RuleError::InvalidPattern`] when `input` is none of that.
    pub fn parse(input: &str) -> Result<Self, RuleError> {
        let valid = input.chars().count() <= Self::MAX_CHARS
            && !input.trim().is_empty()
            && !input.chars().any(char::is_control);
        if valid {
            Ok(Self(input.to_owned()))
        } else {
            Err(RuleError::InvalidPattern)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

string_enum!(
    /// How a rule's text is looked for.
    RuleMatchKind, RuleError::UnknownMatchKind,
    {
        /// Anywhere in the description.
        Contains => "contains",
        /// At the start of the description.
        StartsWith => "starts_with",
    }
);

/// `text` lowercased one character at a time, the same whatever surrounds it. That is
/// all it does: a composed and a decomposed `é` differ, and `ß` isn't `SS`.
fn folded(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// What a rule looks for in a description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleText {
    kind: RuleMatchKind,
    pattern: RulePattern,
    /// `pattern` folded once, since it is compared with every new transaction.
    folded: String,
}

impl RuleText {
    #[must_use]
    pub fn new(kind: RuleMatchKind, pattern: RulePattern) -> Self {
        let folded = folded(pattern.as_str());
        Self {
            kind,
            pattern,
            folded,
        }
    }

    #[must_use]
    pub fn kind(&self) -> RuleMatchKind {
        self.kind
    }

    #[must_use]
    pub fn pattern(&self) -> &RulePattern {
        &self.pattern
    }

    /// Whether `description`, already folded, matches.
    fn found_in(&self, description: &str) -> bool {
        match self.kind {
            RuleMatchKind::Contains => description.contains(&self.folded),
            RuleMatchKind::StartsWith => description.starts_with(&self.folded),
        }
    }
}

/// The signed amounts a rule applies to, in one currency: a number of minor units
/// means nothing without one. Negative is money out of the account, and either end
/// may be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmountRange {
    currency: Currency,
    min_minor: Option<i64>,
    max_minor: Option<i64>,
}

impl AmountRange {
    /// # Errors
    ///
    /// [`RuleError::InvalidAmountRange`] with neither end, or the ends the wrong way
    /// round.
    pub fn new(
        currency: Currency,
        min_minor: Option<i64>,
        max_minor: Option<i64>,
    ) -> Result<Self, RuleError> {
        let ordered = match (min_minor, max_minor) {
            (None, None) => false,
            (Some(min), Some(max)) => min <= max,
            _ => true,
        };
        if !ordered {
            return Err(RuleError::InvalidAmountRange);
        }
        Ok(Self {
            currency,
            min_minor,
            max_minor,
        })
    }

    #[must_use]
    pub fn currency(&self) -> Currency {
        self.currency
    }

    #[must_use]
    pub fn min_minor(&self) -> Option<i64> {
        self.min_minor
    }

    #[must_use]
    pub fn max_minor(&self) -> Option<i64> {
        self.max_minor
    }

    fn holds(&self, currency: Currency, amount_minor: i64) -> bool {
        self.currency == currency
            && self.min_minor.is_none_or(|min| amount_minor >= min)
            && self.max_minor.is_none_or(|max| amount_minor <= max)
    }
}

/// The category a rule files under, as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleCategory {
    pub id: CategoryId,
    pub kind: CategoryKind,
    /// Archived, or in an archived group: it takes no new splits, so the rule
    /// doesn't file under it until it is restored.
    pub archived: bool,
}

/// The member a rule assigns to, as they are now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleAssignee {
    pub id: MemberId,
    /// A disabled member takes no new assignments, so the rule assigns nobody until
    /// they are enabled again.
    pub disabled: bool,
}

/// A rule and what it would fill. `category` or `assignee` is set, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: RuleId,
    pub text: RuleText,
    /// Only this account's transactions; `None` for every account.
    pub account: Option<AccountId>,
    pub amount: Option<AmountRange>,
    pub category: Option<RuleCategory>,
    pub assignee: Option<RuleAssignee>,
}

/// Whether a transaction is one side of a transfer link, and of what kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferLeg {
    NotALeg,
    /// One movement between counted accounts, in the forecast and in reports (§8.12 T5).
    OneMovement,
    /// Any other link, such as to a brokerage: the money really left (§8.11 B9).
    OtherLink,
}

/// What rules read of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleSubject<'a> {
    pub account: AccountId,
    pub amount_minor: i64,
    pub currency: Currency,
    pub description: &'a str,
    /// Nobody has ever categorized it. A person who clears a category has set
    /// something, so that stands.
    pub category_open: bool,
    /// Nobody has ever assigned it, likewise.
    pub assignee_open: bool,
    /// How many splits it has.
    pub splits: usize,
    /// A person or an agent has reviewed it, which covers what it held then.
    pub reviewed: bool,
    pub transfer_leg: TransferLeg,
}

impl Rule {
    fn matches(&self, subject: &RuleSubject<'_>, description: &str) -> bool {
        self.account.is_none_or(|only| only == subject.account)
            && self
                .amount
                .is_none_or(|range| range.holds(subject.currency, subject.amount_minor))
            && self.text.found_in(description)
    }

    /// The category this rule may file `subject` under, whatever else has.
    fn category_for(&self, subject: &RuleSubject<'_>) -> Option<CategoryId> {
        self.category
            .filter(|category| !category.archived)
            .filter(|category| refusal(category.kind, subject).is_none())
            .map(|category| category.id)
    }

    /// The member this rule may assign, whatever else has.
    fn assignee_for(&self) -> Option<MemberId> {
        self.assignee
            .filter(|assignee| !assignee.disabled)
            .map(|assignee| assignee.id)
    }
}

/// Why a category of `kind` would change what `subject` means, if it would. On a transfer leg
/// only a `transfer` category is safe, and only on one movement: an `expense` or `income` one
/// would file a movement as spending or income, and on any other link a `transfer` one would
/// take real spending out of `actual` and off the uncategorized list. An `income` category on
/// money going out would leave spending and that list with no caution. The reverse isn't
/// refused: money coming in under an `expense` category is a refund, which spending nets.
fn refusal(kind: CategoryKind, subject: &RuleSubject<'_>) -> Option<LeftAlone> {
    match (subject.transfer_leg, kind) {
        (TransferLeg::OneMovement, CategoryKind::Transfer) => None,
        (TransferLeg::OneMovement | TransferLeg::OtherLink, _) => Some(LeftAlone::TransferLeg),
        (TransferLeg::NotALeg, CategoryKind::Income) if subject.amount_minor < 0 => {
            Some(LeftAlone::IncomeOnOutflow)
        }
        (TransferLeg::NotALeg, _) => None,
    }
}

/// What rules fill on one transaction, and which rule fills each field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleFills {
    pub category: Option<(RuleId, CategoryId)>,
    pub assignee: Option<(RuleId, MemberId)>,
}

impl RuleFills {
    /// Whether nothing is filled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.category.is_none() && self.assignee.is_none()
    }

    /// Only what `rule` fills.
    #[must_use]
    pub fn by(self, rule: RuleId) -> Self {
        Self {
            category: self.category.filter(|(by, _)| *by == rule),
            assignee: self.assignee.filter(|(by, _)| *by == rule),
        }
    }
}

/// What `rules`, in order, fill on `subject`. The first match for each field wins.
///
/// A transaction with several splits is left alone, as is a reviewed one: someone
/// has divided it or looked at it, and assigning or filing it afterwards would
/// change what they saw. A zero amount can't take a split. A category that would change
/// what the money means isn't one this rule may set ([`refusal`]). A rule like that, or
/// with an archived category or a disabled member, doesn't count as a match for that
/// field, so the next one may fill it.
#[must_use]
pub fn decide(rules: &[Rule], subject: &RuleSubject<'_>) -> RuleFills {
    let mut fills = RuleFills::default();
    if subject.reviewed || subject.splits > 1 {
        return fills;
    }
    let category_open = subject.category_open && subject.amount_minor != 0;
    if !category_open && !subject.assignee_open {
        return fills;
    }
    let description = folded(subject.description);
    for rule in rules
        .iter()
        .filter(|rule| rule.matches(subject, &description))
    {
        if category_open && fills.category.is_none() {
            fills.category = rule.category_for(subject).map(|id| (rule.id, id));
        }
        if subject.assignee_open && fills.assignee.is_none() {
            fills.assignee = rule.assignee_for().map(|id| (rule.id, id));
        }
    }
    fills
}

/// Why a transaction `rule` matches, with a field it could fill still open, stays as
/// it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftAlone {
    Reviewed,
    /// It has several splits.
    Split,
    /// It is a transfer leg, and the rule's category isn't a `transfer` one, or the link
    /// isn't one movement.
    TransferLeg,
    /// Money going out, and the rule's category is an `income` one.
    IncomeOnOutflow,
    ZeroAmount,
    /// An earlier rule fills that field.
    EarlierRule,
}

/// Why `rule`, which fills nothing on `subject`, leaves it alone; `None` when the
/// rule doesn't match or has nothing to offer the fields still open.
#[must_use]
pub fn left_alone(rules: &[Rule], rule: &Rule, subject: &RuleSubject<'_>) -> Option<LeftAlone> {
    let offers_category = subject.category_open && rule.category.is_some_and(|c| !c.archived);
    let offers_assignee = subject.assignee_open && rule.assignee_for().is_some();
    if !(offers_category || offers_assignee) || !rule.matches(subject, &folded(subject.description))
    {
        return None;
    }
    if subject.reviewed {
        return Some(LeftAlone::Reviewed);
    }
    if subject.splits > 1 {
        return Some(LeftAlone::Split);
    }
    let fills = decide(rules, subject);
    if !fills.by(rule.id).is_empty() {
        return None;
    }
    if !offers_assignee {
        if subject.amount_minor == 0 {
            return Some(LeftAlone::ZeroAmount);
        }
        if let Some(reason) = rule.category.and_then(|c| refusal(c.kind, subject)) {
            return Some(reason);
        }
    }
    Some(LeftAlone::EarlierRule)
}

#[cfg(test)]
mod tests;
