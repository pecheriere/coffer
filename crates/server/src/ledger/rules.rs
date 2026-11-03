//! Rules (spec §8.13): keeping them here, and trying or running one in [`run`]. What a
//! rule does is decided only by [`coffer_domain::rules::decide`] and written only by
//! [`crate::canonical::fill_from_rules`]; the sync offers each new transaction to them
//! itself.

pub mod run;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, MemberId, RuleId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{Action, authorize, authorize_rule_change, rule_visible};
use coffer_domain::rules::{AmountRange, MAX_RULES, RuleMatchKind, RulePattern, RuleText};
use coffer_domain::transactions::MAX_POSITION;
use coffer_store::{RuleFields, RuleRecord, StoreError, StoreTx};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::transactions::{Patch, check_assignee};
use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;

const PATTERN_INVALID: &str = "pattern must be 1 to 200 characters without control characters";
const POSITION_INVALID: &str = "position must be at most 10000";
const AMOUNT_INVALID: &str = "an amount range needs a lowest or a highest amount, lowest first";
const AMOUNT_CURRENCY: &str = "an amount range must be in the currency of the rule's account";
const NOTHING_TO_SET: &str = "a rule sets a category, an assignee or both";
const UNKNOWN_CATEGORY: &str = "the category does not exist";
const ARCHIVED_CATEGORY: &str = "the category is archived";
const TOO_MANY: &str = "a household has at most 200 rules";

/// The signed amounts a rule applies to, in one currency. Negative is money out of
/// the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleAmount {
    pub currency: Currency,
    /// The lowest amount, in minor units; `null` for no lowest.
    #[serde(default)]
    #[schema(required)]
    pub min_minor: Option<i64>,
    /// The highest amount, in minor units; `null` for no highest.
    #[serde(default)]
    #[schema(required)]
    pub max_minor: Option<i64>,
}

impl RuleAmount {
    fn range(self) -> Result<AmountRange, LedgerError> {
        AmountRange::new(self.currency, self.min_minor, self.max_minor)
            .map_err(|_| LedgerError::Invalid(AMOUNT_INVALID))
    }
}

impl From<AmountRange> for RuleAmount {
    fn from(range: AmountRange) -> Self {
        Self {
            currency: range.currency(),
            min_minor: range.min_minor(),
            max_minor: range.max_minor(),
        }
    }
}

/// A rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleView {
    pub id: RuleId,
    pub match_kind: RuleMatchKind,
    /// The text looked for in the description, ignoring case. Plain text: nothing in
    /// it is a wildcard.
    pub pattern: String,
    /// Only this account's transactions; `null` for every account.
    #[schema(required)]
    pub account: Option<AccountId>,
    /// Only amounts in this range; `null` for any.
    #[schema(required)]
    pub amount: Option<RuleAmount>,
    /// The category it files under; `null` if it doesn't.
    #[schema(required)]
    pub category: Option<CategoryId>,
    /// The category, or its group, is archived, so the rule files nothing until it is
    /// restored, and a later rule may.
    pub category_archived: bool,
    /// The member it assigns; `null` if it doesn't.
    #[schema(required)]
    pub assignee: Option<MemberId>,
    /// The member is disabled, so the rule assigns nobody until they are enabled, and
    /// a later rule may.
    pub assignee_disabled: bool,
    /// Its place in the order: lower first, ties oldest first. For each field, the
    /// first rule that matches wins.
    pub position: u32,
    /// Whether it runs on new transactions. Off, it can still be tried and run by
    /// hand.
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl RuleView {
    fn new(record: &RuleRecord) -> Self {
        let rule = &record.rule;
        Self {
            id: rule.id,
            match_kind: rule.text.kind(),
            pattern: rule.text.pattern().as_str().to_owned(),
            account: rule.account,
            amount: rule.amount.map(RuleAmount::from),
            category: rule.category.map(|category| category.id),
            category_archived: rule.category.is_some_and(|category| category.archived),
            assignee: rule.assignee.map(|assignee| assignee.id),
            assignee_disabled: rule.assignee.is_some_and(|assignee| assignee.disabled),
            position: record.position,
            enabled: record.enabled,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// The rules the token may see, in the order they run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleList {
    pub items: Vec<RuleView>,
}

/// A rule to create.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleInput {
    pub match_kind: RuleMatchKind,
    /// 1 to 200 characters, without control characters. Plain text, matched ignoring
    /// case.
    pub pattern: String,
    /// Only this account's transactions. Without one the rule is for every account,
    /// which needs write on every account.
    pub account: Option<AccountId>,
    /// Only amounts in this range. With an `account`, in its currency.
    pub amount: Option<RuleAmount>,
    /// The category to file under: one split of the whole amount.
    pub category: Option<CategoryId>,
    /// The active member to assign.
    pub assignee: Option<MemberId>,
    /// 0 to 10000; by default after the last rule.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
    /// Whether it runs on new transactions; by default it does. Create one off to try
    /// it first.
    pub enabled: Option<bool>,
}

/// Changes to a rule; an absent field stays as it is. What it already did stays too.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RulePatch {
    pub match_kind: Option<RuleMatchKind>,
    /// 1 to 200 characters, without control characters.
    pub pattern: Option<String>,
    /// `null` makes it a rule for every account, which needs write on every account.
    #[serde(default)]
    #[schema(value_type = Option<AccountId>)]
    pub account: Patch<AccountId>,
    /// `null` drops the range.
    #[serde(default)]
    #[schema(value_type = Option<RuleAmount>)]
    pub amount: Patch<RuleAmount>,
    /// `null` stops it filing. A rule keeps a category or an assignee.
    #[serde(default)]
    #[schema(value_type = Option<CategoryId>)]
    pub category: Patch<CategoryId>,
    /// `null` stops it assigning. A rule keeps a category or an assignee.
    #[serde(default)]
    #[schema(value_type = Option<MemberId>)]
    pub assignee: Patch<MemberId>,
    /// 0 to 10000.
    #[schema(maximum = 10000)]
    pub position: Option<u32>,
    pub enabled: Option<bool>,
}

fn pattern(input: &str) -> Result<RulePattern, LedgerError> {
    RulePattern::parse(input).map_err(|_| LedgerError::Invalid(PATTERN_INVALID))
}

fn check_position(position: u32) -> Result<u32, LedgerError> {
    if position > MAX_POSITION {
        return Err(LedgerError::Invalid(POSITION_INVALID));
    }
    Ok(position)
}

/// Refuses a category that does not exist or takes no new splits.
async fn check_category(tx: &mut StoreTx, id: CategoryId) -> Result<(), LedgerError> {
    let category = match tx.get_category(id).await {
        Ok(category) => category,
        Err(StoreError::NotFound) => return Err(LedgerError::Invalid(UNKNOWN_CATEGORY)),
        Err(other) => return Err(other.into()),
    };
    let group = tx.get_category_group(category.group).await?;
    if category.archived_at.is_some() || group.archived_at.is_some() {
        return Err(LedgerError::Invalid(ARCHIVED_CATEGORY));
    }
    Ok(())
}

/// Refuses an account that does not exist, answered like a hidden one, and a range in
/// another currency than the account's, which could never match.
async fn check_scope(tx: &mut StoreTx, fields: &RuleFields) -> Result<(), LedgerError> {
    if let Some(account) = fields.account {
        let account = tx.get_account(account).await?;
        if fields
            .amount
            .is_some_and(|amount| amount.currency() != account.currency)
        {
            return Err(LedgerError::Invalid(AMOUNT_CURRENCY));
        }
    }
    Ok(())
}

fn audit_fields(changes: AuditChanges, fields: &RuleFields) -> AuditChanges {
    changes
        .set("match_kind", fields.text.kind().as_str())
        .set("pattern", fields.text.pattern().as_str())
        .set("account", fields.account)
        .set(
            "amount_currency",
            fields.amount.map(|a| a.currency().as_str()),
        )
        .set(
            "amount_min_minor",
            fields.amount.and_then(|a| a.min_minor()),
        )
        .set(
            "amount_max_minor",
            fields.amount.and_then(|a| a.max_minor()),
        )
        .set("category", fields.category)
        .set("assignee", fields.assignee)
        .set("position", i64::from(fields.position))
        .set("enabled", fields.enabled)
}

/// What `before` and `after` differ in.
fn audit_difference(before: &RuleFields, after: &RuleFields) -> AuditChanges {
    let mut changes = AuditChanges::new();
    if before.text.kind() != after.text.kind() {
        changes = changes.change(
            "match_kind",
            before.text.kind().as_str(),
            after.text.kind().as_str(),
        );
    }
    if before.text.pattern() != after.text.pattern() {
        changes = changes.change(
            "pattern",
            before.text.pattern().as_str(),
            after.text.pattern().as_str(),
        );
    }
    if before.account != after.account {
        changes = changes.change("account", before.account, after.account);
    }
    if before.amount != after.amount {
        changes = changes
            .change(
                "amount_currency",
                before.amount.map(|a| a.currency().as_str()),
                after.amount.map(|a| a.currency().as_str()),
            )
            .change(
                "amount_min_minor",
                before.amount.and_then(|a| a.min_minor()),
                after.amount.and_then(|a| a.min_minor()),
            )
            .change(
                "amount_max_minor",
                before.amount.and_then(|a| a.max_minor()),
                after.amount.and_then(|a| a.max_minor()),
            );
    }
    if before.category != after.category {
        changes = changes.change("category", before.category, after.category);
    }
    if before.assignee != after.assignee {
        changes = changes.change("assignee", before.assignee, after.assignee);
    }
    if before.position != after.position {
        changes = changes.change(
            "position",
            i64::from(before.position),
            i64::from(after.position),
        );
    }
    if before.enabled != after.enabled {
        changes = changes.change("enabled", before.enabled, after.enabled);
    }
    changes
}

fn fields_of(record: &RuleRecord) -> RuleFields {
    let rule = &record.rule;
    RuleFields {
        text: rule.text.clone(),
        account: rule.account,
        amount: rule.amount,
        category: rule.category.map(|category| category.id),
        assignee: rule.assignee.map(|assignee| assignee.id),
        position: record.position,
        enabled: record.enabled,
    }
}

/// `patch` over `current`.
fn patched<T: Copy>(patch: &Patch<T>, current: Option<T>) -> Option<T> {
    match patch {
        Patch::Absent => current,
        Patch::Clear => None,
        Patch::Set(value) => Some(*value),
    }
}

impl Ledger {
    /// Whether `actor` may create, change, delete or run a rule for `account`, decided
    /// before a create reserves its idempotency key. Says nothing of whether the
    /// account exists.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`] and write on the
    /// account, or on every account for a rule of all of them; a failure to resolve
    /// access.
    pub async fn authorize_rule_input(
        &self,
        actor: &Actor,
        account: Option<AccountId>,
    ) -> Result<(), LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_rule_change(actor, &access, account)?;
        Ok(())
    }

    /// The rules `actor` may see, in the order they run.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage failures.
    pub async fn list_rules(&self, actor: &Actor) -> Result<RuleList, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let records = self.store.list_rules().await?;
        Ok(RuleList {
            items: records
                .iter()
                .filter(|record| rule_visible(&access, record.rule.account))
                .map(RuleView::new)
                .collect(),
        })
    }

    /// One rule.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown rule or one `actor` may not see;
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage failures.
    pub async fn get_rule(&self, actor: &Actor, id: RuleId) -> Result<RuleView, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let record = self.store.get_rule(id).await?;
        if !rule_visible(&access, record.rule.account) {
            return Err(LedgerError::NotFound);
        }
        Ok(RuleView::new(&record))
    }

    /// Creates a rule. With `slot`, the idempotency key is completed with the response
    /// in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] or [`LedgerError::NotFound`] as
    /// [`Self::authorize_rule_input`], and for an unknown account;
    /// [`LedgerError::Invalid`] for an invalid value, a category or member that can't
    /// take new work, or one rule too many; storage failures.
    pub async fn create_rule(
        &self,
        actor: &Actor,
        input: RuleInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<RuleView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_rule_change(actor, &access, input.account)?;
        let text = RuleText::new(input.match_kind, pattern(&input.pattern)?);
        let amount = input.amount.map(RuleAmount::range).transpose()?;
        let position = input.position.map(check_position).transpose()?;
        if input.category.is_none() && input.assignee.is_none() {
            return Err(LedgerError::Invalid(NOTHING_TO_SET));
        }
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let existing = tx.list_rules().await?;
        if existing.len() >= MAX_RULES {
            return Err(LedgerError::Invalid(TOO_MANY));
        }
        // After the last one the actor can see: a rule it can't see can't compete with this
        // one, and its place is none of its business.
        let last_seen = existing
            .iter()
            .filter(|record| rule_visible(&access, record.rule.account))
            .map(|record| record.position)
            .max();
        let position = position.unwrap_or_else(|| {
            last_seen.map_or(0, |last| last.saturating_add(1).min(MAX_POSITION))
        });
        let fields = RuleFields {
            text,
            account: input.account,
            amount,
            category: input.category,
            assignee: input.assignee,
            position,
            enabled: input.enabled.unwrap_or(true),
        };
        check_scope(&mut tx, &fields).await?;
        if let Some(category) = fields.category {
            check_category(&mut tx, category).await?;
        }
        if let Some(assignee) = fields.assignee {
            check_assignee(&mut tx, assignee).await?;
        }
        let record = tx.insert_rule(&fields, now).await?;
        let changes = audit_fields(AuditChanges::new(), &fields);
        audit(
            &mut tx,
            now,
            actor,
            "rule.create",
            "rule",
            record.rule.id,
            changes,
        )
        .await?;
        let view = RuleView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a rule, which needs the right to change it as it is and as it would
    /// be. A patch that changes nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown rule or one `actor` may not see, and
    /// for an unknown account; [`LedgerError::Denied`] without the right;
    /// [`LedgerError::Invalid`] for an invalid value, or a new category or member that
    /// can't take new work; storage failures.
    pub async fn update_rule(
        &self,
        actor: &Actor,
        id: RuleId,
        patch: RulePatch,
    ) -> Result<RuleView, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let new_pattern = patch.pattern.as_deref().map(pattern).transpose()?;
        let new_amount = match patch.amount {
            Patch::Absent => Patch::Absent,
            Patch::Clear => Patch::Clear,
            Patch::Set(amount) => Patch::Set(amount.range()?),
        };
        let new_position = patch.position.map(check_position).transpose()?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_rule(id).await?;
        if !rule_visible(&access, current.rule.account) {
            return Err(LedgerError::NotFound);
        }
        authorize_rule_change(actor, &access, current.rule.account)?;
        let before = fields_of(&current);
        let after = RuleFields {
            text: RuleText::new(
                patch.match_kind.unwrap_or(before.text.kind()),
                new_pattern.unwrap_or_else(|| before.text.pattern().clone()),
            ),
            account: patched(&patch.account, before.account),
            amount: patched(&new_amount, before.amount),
            category: patched(&patch.category, before.category),
            assignee: patched(&patch.assignee, before.assignee),
            position: new_position.unwrap_or(before.position),
            enabled: patch.enabled.unwrap_or(before.enabled),
        };
        if after.category.is_none() && after.assignee.is_none() {
            return Err(LedgerError::Invalid(NOTHING_TO_SET));
        }
        if after.account != before.account {
            authorize_rule_change(actor, &access, after.account)?;
        }
        if after.account != before.account || after.amount != before.amount {
            check_scope(&mut tx, &after).await?;
        }
        if after.category != before.category
            && let Some(category) = after.category
        {
            check_category(&mut tx, category).await?;
        }
        if after.assignee != before.assignee
            && let Some(assignee) = after.assignee
        {
            check_assignee(&mut tx, assignee).await?;
        }
        let changes = audit_difference(&before, &after);
        if changes.is_empty() {
            return Ok(RuleView::new(&current));
        }
        let record = tx.update_rule(id, &after, now).await?;
        audit(&mut tx, now, actor, "rule.update", "rule", id, changes).await?;
        tx.commit().await?;
        Ok(RuleView::new(&record))
    }

    /// Deletes a rule. What it did stays in place, still naming it.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown rule or one `actor` may not see;
    /// [`LedgerError::Denied`] without the right to change it; storage failures.
    pub async fn delete_rule(&self, actor: &Actor, id: RuleId) -> Result<(), LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_rule(id).await?;
        if !rule_visible(&access, current.rule.account) {
            return Err(LedgerError::NotFound);
        }
        authorize_rule_change(actor, &access, current.rule.account)?;
        tx.delete_rule(id).await?;
        let fields = fields_of(&current);
        let empty = RuleFields {
            account: None,
            amount: None,
            category: None,
            assignee: None,
            ..fields.clone()
        };
        let changes = audit_difference(&fields, &empty)
            .unset("match_kind", fields.text.kind().as_str())
            .unset("pattern", fields.text.pattern().as_str())
            .unset("position", i64::from(fields.position))
            .unset("enabled", fields.enabled);
        audit(&mut tx, now, actor, "rule.delete", "rule", id, changes).await?;
        tx.commit().await?;
        Ok(())
    }
}
