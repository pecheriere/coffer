//! Trying a rule on the transactions already held, and running one by hand in short
//! batches.

use std::time::Duration;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, MemberId, RuleId, TransactionId};
use coffer_domain::money::Money;
use coffer_domain::policy::{
    AccountLevel, AccountScope, Action, authorize, authorize_rule_change, rule_visible,
};
use coffer_domain::rules::{LeftAlone, Rule, RuleFills, decide, left_alone};
use coffer_store::{RuleRecord, StoreError, TransactionRecord};
use jiff::civil::Date;
use serde::Serialize;
use utoipa::ToSchema;

use crate::canonical::fill_from_rules;
use crate::ledger::transactions::attribution;
use crate::ledger::{Ledger, LedgerError, audit};

/// The most changes a dry run lists.
const MAX_DRY_RUN_LIMIT: u32 = 200;
/// How many it lists unless asked.
const DEFAULT_DRY_RUN_LIMIT: u32 = 50;
/// Transactions read at a time while going through a history, without the write lock.
const READ_PAGE: u32 = 500;
/// Transactions changed in one write transaction, so a long run never holds the write
/// lock for long.
const WRITE_BATCH: usize = 100;
/// The rest between two write transactions, which lets a sync or a member in.
const PAUSE_BETWEEN_BATCHES: Duration = Duration::from_millis(25);

/// One change a rule would make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleChangeView {
    pub transaction: TransactionId,
    pub account: AccountId,
    pub posted_on: Date,
    /// Negative is money out of the account.
    pub amount: Money,
    /// The description as its source gave it.
    pub description: String,
    /// The category it would file under; `null` if it wouldn't.
    #[schema(required)]
    pub category: Option<CategoryId>,
    /// The member it would assign; `null` if it wouldn't.
    #[schema(required)]
    pub assignee: Option<MemberId>,
}

/// Transactions the rule matches, with a field it could fill still open, that it
/// leaves as they are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleLeftAlone {
    /// Someone has reviewed them.
    pub reviewed: u32,
    /// They have several splits.
    pub split: u32,
    /// They are transfer legs, and the rule's category isn't a `transfer` one.
    pub transfer_leg: u32,
    /// A zero amount can't take a split.
    pub zero_amount: u32,
    /// An earlier rule fills that field: run that one.
    pub earlier_rule: u32,
}

impl RuleLeftAlone {
    fn count(&mut self, reason: LeftAlone) {
        let count = match reason {
            LeftAlone::Reviewed => &mut self.reviewed,
            LeftAlone::Split => &mut self.split,
            LeftAlone::TransferLeg => &mut self.transfer_leg,
            LeftAlone::ZeroAmount => &mut self.zero_amount,
            LeftAlone::EarlierRule => &mut self.earlier_rule,
        };
        *count += 1;
    }
}

/// What running a rule by hand would change now, among the transactions the token
/// can read. Nothing is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleDryRun {
    /// The first of the changes, up to `limit`, oldest transaction first.
    pub changes: Vec<RuleChangeView>,
    /// How many transactions it would change, listed or not.
    pub would_change: u32,
    /// How many of them it would file under its category.
    pub would_categorize: u32,
    /// How many of them it would assign.
    pub would_assign: u32,
    pub left_alone: RuleLeftAlone,
}

/// What running a rule by hand changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleApplied {
    /// How many transactions it changed.
    pub changed: u32,
    /// How many of them it filed under its category.
    pub categorized: u32,
    /// How many of them it assigned.
    pub assigned: u32,
    pub left_alone: RuleLeftAlone,
}

impl RuleApplied {
    fn count(&mut self, fills: RuleFills) {
        self.changed += 1;
        self.categorized += u32::from(fills.category.is_some());
        self.assigned += u32::from(fills.assignee.is_some());
    }
}

/// The rules that decide a manual run of `target`, in order: those that run on new
/// transactions, and `target` itself even when off. Running rules one at a time, in
/// any order, then gives what running them together would. `None` when it is gone.
fn run_order(records: &[RuleRecord], target: RuleId) -> Option<Vec<Rule>> {
    records
        .iter()
        .any(|record| record.rule.id == target)
        .then(|| {
            records
                .iter()
                .filter(|record| record.enabled || record.rule.id == target)
                .map(|record| record.rule.clone())
                .collect()
        })
}

impl Ledger {
    /// What running a rule by hand would change now, reading only transactions `actor`
    /// may read, so that neither the list nor a count says anything of the others.
    /// Writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown rule or one `actor` may not see;
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage failures.
    pub async fn dry_run_rule(
        &self,
        actor: &Actor,
        id: RuleId,
        limit: Option<u32>,
    ) -> Result<RuleDryRun, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let records = self.store.list_rules().await?;
        let target = records
            .iter()
            .find(|record| record.rule.id == id)
            .filter(|record| rule_visible(&access, record.rule.account))
            .ok_or(LedgerError::NotFound)?;
        let Some(rules) = run_order(&records, id) else {
            return Err(LedgerError::NotFound);
        };
        let limit = limit
            .unwrap_or(DEFAULT_DRY_RUN_LIMIT)
            .clamp(1, MAX_DRY_RUN_LIMIT) as usize;
        let scope = access.scope(AccountLevel::Transactions);
        let mut result = RuleDryRun {
            changes: Vec::new(),
            would_change: 0,
            would_categorize: 0,
            would_assign: 0,
            left_alone: RuleLeftAlone::default(),
        };
        let mut after = None;
        loop {
            let page = self
                .store
                .transactions_with_open_fields(&scope, target.rule.account, after, READ_PAGE)
                .await?;
            for record in &page {
                let subject = record.rule_subject();
                let fills = decide(&rules, &subject).by(id);
                if fills.is_empty() {
                    if let Some(reason) = left_alone(&rules, &target.rule, &subject) {
                        result.left_alone.count(reason);
                    }
                    continue;
                }
                result.would_change += 1;
                result.would_categorize += u32::from(fills.category.is_some());
                result.would_assign += u32::from(fills.assignee.is_some());
                if result.changes.len() < limit {
                    result.changes.push(change_view(record, fills));
                }
            }
            after = page.last().map(|record| record.id);
            if page.len() < READ_PAGE as usize {
                return Ok(result);
            }
        }
    }

    /// Runs a rule by hand over the transactions it can reach: filling only what nobody
    /// has set, in short write transactions, each holding its own audit entries and a
    /// `rule.apply` entry with its counts. Stopped partway, what is done stays done and
    /// is audited, and running it again carries on.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown rule or one `actor` may not see;
    /// [`LedgerError::Denied`] without the right to change it; storage failures.
    pub async fn apply_rule(&self, actor: &Actor, id: RuleId) -> Result<RuleApplied, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let current = self.store.get_rule(id).await?;
        if !rule_visible(&access, current.rule.account) {
            return Err(LedgerError::NotFound);
        }
        authorize_rule_change(actor, &access, current.rule.account)?;
        let scope = AccountScope::All;
        let account = current.rule.account;
        let mut result = RuleApplied::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .transactions_with_open_fields(&scope, account, after, READ_PAGE)
                .await?;
            let records = self.store.list_rules().await?;
            let target = records.iter().find(|record| record.rule.id == id);
            let (Some(target), Some(rules)) = (target, run_order(&records, id)) else {
                return Ok(result);
            };
            let mut filling = Vec::new();
            for record in &page {
                let subject = record.rule_subject();
                if decide(&rules, &subject).by(id).is_empty() {
                    if let Some(reason) = left_alone(&rules, &target.rule, &subject) {
                        result.left_alone.count(reason);
                    }
                } else {
                    filling.push(record.id);
                }
            }
            for batch in filling.chunks(WRITE_BATCH) {
                self.fill_batch(actor, id, batch, &mut result).await?;
                tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
            }
            after = page.last().map(|record| record.id);
            if page.len() < READ_PAGE as usize {
                return Ok(result);
            }
        }
    }

    /// One write transaction of [`Self::apply_rule`]: decides again under the write
    /// lock, since a person may have got there first.
    async fn fill_batch(
        &self,
        actor: &Actor,
        id: RuleId,
        batch: &[TransactionId],
        result: &mut RuleApplied,
    ) -> Result<(), LedgerError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let records = tx.list_rules().await?;
        let Some(rules) = run_order(&records, id) else {
            return Ok(());
        };
        let by = attribution(actor, now);
        let mut done = RuleApplied::default();
        for transaction in batch {
            let current = match tx.get_transaction(*transaction).await {
                Ok(current) => current,
                Err(StoreError::NotFound) => continue,
                Err(other) => return Err(other.into()),
            };
            let fills = decide(&rules, &current.rule_subject()).by(id);
            if fills.is_empty() {
                continue;
            }
            fill_from_rules(&mut tx, now, &actor.into(), &by, &current, fills).await?;
            done.count(fills);
        }
        if done.changed > 0 {
            let changes = AuditChanges::new()
                .set("transactions", i64::from(done.changed))
                .set("categorized", i64::from(done.categorized))
                .set("assigned", i64::from(done.assigned));
            audit(&mut tx, now, actor, "rule.apply", "rule", id, changes).await?;
        }
        tx.commit().await?;
        result.changed += done.changed;
        result.categorized += done.categorized;
        result.assigned += done.assigned;
        Ok(())
    }
}

fn change_view(record: &TransactionRecord, fills: RuleFills) -> RuleChangeView {
    RuleChangeView {
        transaction: record.id,
        account: record.account,
        posted_on: record.posted_on,
        amount: Money {
            minor_units: record.amount_minor,
            currency: record.currency,
        },
        description: record.raw_description.as_str().to_owned(),
        category: fills.category.map(|(_, category)| category),
        assignee: fills.assignee.map(|(_, member)| member),
    }
}
