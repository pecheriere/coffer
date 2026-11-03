//! Taking back what a rule filled (spec §8.13 U11), in the same short batches as a run.
//! Each field reopens, which clearing it by hand wouldn't: that says it stays empty, so a
//! corrected rule would skip it.

use std::time::Duration;

use coffer_domain::audit::{AuditActor, AuditChanges};
use coffer_domain::identity::Actor;
use coffer_domain::ids::{RuleId, TransactionId};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, AccountScope, Action, authorize, authorize_account,
    authorize_rule_change, rule_visible,
};
use coffer_domain::transactions::ReviewStatus;
use coffer_store::StoreError;
use serde::Serialize;
use utoipa::ToSchema;

use crate::canonical::undo_rule_fills;
use crate::ledger::{Ledger, LedgerError, audit};

/// Transactions read at a time, without the write lock.
const READ_PAGE: u32 = 500;
/// Transactions changed in one write transaction.
const WRITE_BATCH: usize = 100;
/// The rest between two write transactions.
const PAUSE_BETWEEN_BATCHES: Duration = Duration::from_millis(25);

/// What taking back a rule's work changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleUndone {
    /// How many transactions it changed.
    pub changed: u32,
    /// How many categories it took back.
    pub categorized: u32,
    /// How many assignees it took back.
    pub assigned: u32,
    /// How many it left, because someone has reviewed them: that is for a person to change.
    pub reviewed: u32,
}

impl Ledger {
    /// The accounts `actor` may undo `id`'s work on, as things stand now: every account with
    /// write on all of them, which also reaches an account the rule has since moved off, or
    /// otherwise the rule's own. A deleted rule's account isn't known, so it needs write on
    /// all of them, and below that it answers as missing.
    async fn undo_scope(
        &self,
        actor: &Actor,
        access: &AccountAccess,
        id: RuleId,
    ) -> Result<AccountScope, LedgerError> {
        match self.store.get_rule(id).await {
            Ok(record) => {
                if !rule_visible(access, record.rule.account) {
                    return Err(LedgerError::NotFound);
                }
                authorize_rule_change(actor, access, record.rule.account)?;
                if access.sees_every_account(AccountLevel::Write) {
                    return Ok(AccountScope::All);
                }
                Ok(AccountScope::Only(
                    record.rule.account.into_iter().collect(),
                ))
            }
            Err(StoreError::NotFound) => {
                authorize_rule_change(actor, access, None).map_err(|_| LedgerError::NotFound)?;
                Ok(AccountScope::All)
            }
            Err(other) => Err(other.into()),
        }
    }

    /// Takes back what rule `id` filled and nobody has changed since, reopening each field.
    /// Each write transaction holds its own audit entries and a `rule.undo` entry with its
    /// counts. Stopped partway, what is done stays done, and asking again carries on.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for a rule `actor` may not see, and for a deleted or unknown
    /// one without write on every account; [`LedgerError::Denied`] without the right to change
    /// it, or on losing it; [`LedgerError::Auth`] when the actor stops holding; storage failures.
    pub async fn undo_rule(&self, actor: &Actor, id: RuleId) -> Result<RuleUndone, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let scope = self.undo_scope(actor, &access, id).await?;
        let mut result = RuleUndone::default();
        let mut after = None;
        loop {
            let page = self
                .store
                .transactions_filled_by_rule(&scope, id, after, READ_PAGE)
                .await?;
            let ids: Vec<TransactionId> = page.iter().map(|record| record.id).collect();
            for batch in ids.chunks(WRITE_BATCH) {
                let done = self.undo_rule_batch(actor, id, batch).await?;
                result.changed += done.changed;
                result.categorized += done.categorized;
                result.assigned += done.assigned;
                result.reviewed += done.reviewed;
                tokio::time::sleep(PAUSE_BETWEEN_BATCHES).await;
            }
            after = page.last().map(|record| record.id);
            if page.len() < READ_PAGE as usize {
                return Ok(result);
            }
        }
    }

    /// One write transaction of [`Self::undo_rule`]: asks again whether the actor may, on each
    /// transaction's own account, and takes back only what still names the rule.
    ///
    /// # Errors
    ///
    /// As [`Self::undo_rule`].
    pub async fn undo_rule_batch(
        &self,
        actor: &Actor,
        id: RuleId,
        batch: &[TransactionId],
    ) -> Result<RuleUndone, LedgerError> {
        let now = self.clock.now();
        let actor = self.access.current(actor, now).await?;
        let access = self.access.resolve(&actor).await?;
        self.undo_scope(&actor, &access, id).await?;
        let auditor = AuditActor::from(&actor);
        let mut tx = self.store.begin().await?;
        let mut done = RuleUndone::default();
        for transaction in batch {
            let current = match tx.get_transaction(*transaction).await {
                Ok(current) => current,
                Err(StoreError::NotFound) => continue,
                Err(other) => return Err(other.into()),
            };
            authorize_account(&actor, &access, current.account, AccountLevel::Write)?;
            if current.review == ReviewStatus::Reviewed {
                done.reviewed += 1;
                continue;
            }
            let (category, assignee) =
                undo_rule_fills(&mut tx, now, &auditor, &current, id).await?;
            if category || assignee {
                done.changed += 1;
                done.categorized += u32::from(category);
                done.assigned += u32::from(assignee);
            }
        }
        if done.changed > 0 {
            let changes = AuditChanges::new()
                .set("transactions", i64::from(done.changed))
                .set("categorized", i64::from(done.categorized))
                .set("assigned", i64::from(done.assigned));
            audit(&mut tx, now, &actor, "rule.undo", "rule", id, changes).await?;
        }
        tx.commit().await?;
        Ok(done)
    }
}
