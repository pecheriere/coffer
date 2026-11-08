//! A patch to a planned item: its values checked before the write lock is taken, then
//! against the item it would leave, giving what changes and its audit record.

use coffer_domain::audit::AuditChanges;
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::money::Money;
use coffer_domain::plan::{
    PlanDirection, PlanSchedule, PlanSpending, PlanTier, PlannedItemName, resolved_tier,
    settlement_applies, validate_amount,
};
use coffer_domain::transactions::Notes;
use coffer_store::{PlannedItemChanges, PlannedItemRecord, StoreTx};

use super::{
    GOAL_LINKED_CURRENCY, PlannedItemPatch, SETTLED, TYPE_CHANGED, check_category, check_hint,
    invalid, resolved_spending, schedule_text,
};
use crate::http::problem::ProblemCode;
use crate::ledger::LedgerError;
use crate::ledger::transactions::{Patch, notes};

/// `audited` with what differs between the amounts `from` and `to`.
fn amount_audited(mut audited: AuditChanges, from: Money, to: Money) -> AuditChanges {
    if to.minor_units != from.minor_units {
        audited = audited.change("amount_minor", from.minor_units, to.minor_units);
    }
    if to.currency != from.currency {
        audited = audited.change("currency", from.currency.as_str(), to.currency.as_str());
    }
    audited
}

/// A patch's text and amount, validated before the write lock is taken.
pub(super) struct CheckedPatch {
    name: Option<PlannedItemName>,
    amount: Option<Money>,
    notes: Patch<Notes>,
    category: Option<CategoryId>,
    direction: Option<PlanDirection>,
    account_hint: Patch<AccountId>,
    active: Option<bool>,
    schedule: Option<PlanSchedule>,
    spending: Option<PlanSpending>,
    tier: Option<PlanTier>,
}

impl CheckedPatch {
    pub(super) fn new(patch: PlannedItemPatch) -> Result<Self, LedgerError> {
        let name = patch
            .name
            .as_deref()
            .map(PlannedItemName::parse)
            .transpose()
            .map_err(invalid)?;
        if let Some(amount) = patch.amount {
            validate_amount(amount).map_err(invalid)?;
        }
        // Empty notes clear them.
        let notes = match &patch.notes {
            Patch::Absent => Patch::Absent,
            Patch::Clear => Patch::Clear,
            Patch::Set(value) => notes(value)?.map_or(Patch::Clear, Patch::Set),
        };
        Ok(Self {
            name,
            amount: patch.amount,
            notes,
            category: patch.category,
            direction: patch.direction,
            account_hint: patch.account_hint,
            active: patch.active,
            schedule: patch.schedule,
            spending: patch.spending,
            tier: patch.tier,
        })
    }

    /// Checks what this patch changes against the item it would leave, and
    /// returns what differs from `current` with its audit record. What it
    /// leaves alone isn't checked again: an item whose category has since been
    /// archived can still be renamed.
    pub(super) async fn changes(
        self,
        tx: &mut StoreTx,
        current: &PlannedItemRecord,
    ) -> Result<(PlannedItemChanges, AuditChanges), LedgerError> {
        let direction = self.direction.unwrap_or(current.direction);
        let category = self.category.unwrap_or(current.category);
        let schedule = self.schedule.unwrap_or(current.schedule);
        if schedule.item_type() != current.schedule.item_type() {
            return Err(LedgerError::Invalid(TYPE_CHANGED));
        }
        let direction_changed = direction != current.direction;
        if direction_changed || schedule != current.schedule {
            schedule.validate(direction).map_err(invalid)?;
        }
        if direction_changed || category != current.category {
            check_category(tx, category, direction).await?;
        }

        let spending = resolved_spending(
            self.spending,
            current.spending.unwrap_or(PlanSpending::Bill),
            direction,
            &schedule,
        )?;
        // An item staying an outflow keeps its tier unless one is sent; one becoming an
        // outflow takes its kind's default, and one becoming income loses it.
        let kept = if direction == current.direction {
            current.tier
        } else {
            None
        };
        let tier =
            resolved_tier(self.tier.or(kept), direction, &schedule, spending).map_err(invalid)?;
        // A settlement says only what a bill or a one-off can have said of it.
        if !settlement_applies(direction, &schedule, spending)
            && settlement_applies(current.direction, &current.schedule, current.spending)
            && !tx.settlements_of_item(current.id).await?.is_empty()
        {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, SETTLED));
        }

        let mut changes = PlannedItemChanges::default();
        let mut audited = AuditChanges::new();
        if let Some(name) = self.name.filter(|name| *name != current.name) {
            audited = audited.change("name", current.name.as_str(), name.as_str());
            changes.name = Some(name);
        }
        if category != current.category {
            audited = audited.change("category", current.category, category);
            changes.category = Some(category);
        }
        if let Some(amount) = self.amount.filter(|amount| *amount != current.amount) {
            // A link promises the goal's currency is the transaction's.
            if amount.currency != current.amount.currency && tx.goal_has_links(current.id).await? {
                return Err(LedgerError::Conflict(
                    ProblemCode::Conflict,
                    GOAL_LINKED_CURRENCY,
                ));
            }
            audited = amount_audited(audited, current.amount, amount);
            changes.amount = Some(amount);
        }
        if direction_changed {
            audited = audited.change("direction", current.direction.as_str(), direction.as_str());
            changes.direction = Some(direction);
        }
        let hint = match self.account_hint {
            Patch::Absent => current.account_hint,
            Patch::Clear => None,
            Patch::Set(account) => Some(account),
        };
        if hint != current.account_hint {
            if let Some(account) = hint {
                check_hint(tx.get_account(account).await)?;
            }
            audited = audited.change("account_hint", current.account_hint, hint);
            changes.account_hint = Some(hint);
        }
        let new_notes = match self.notes {
            Patch::Absent => current.notes.clone(),
            Patch::Clear => None,
            Patch::Set(notes) => Some(notes),
        };
        if new_notes != current.notes {
            audited = audited.change(
                "notes",
                current.notes.as_ref().map(Notes::as_str),
                new_notes.as_ref().map(Notes::as_str),
            );
            changes.notes = Some(new_notes);
        }
        if let Some(active) = self.active.filter(|active| *active != current.active) {
            audited = audited.change("active", current.active, active);
            changes.active = Some(active);
        }
        if schedule != current.schedule {
            audited = audited.change(
                "schedule",
                schedule_text(&current.schedule)?,
                schedule_text(&schedule)?,
            );
            changes.schedule = Some(schedule);
        }
        if spending != current.spending {
            audited = audited.change(
                "spending",
                current.spending.map(PlanSpending::as_str),
                spending.map(PlanSpending::as_str),
            );
            changes.spending = Some(spending);
        }
        if tier != current.tier {
            audited = audited.change(
                "tier",
                current.tier.map(PlanTier::as_str),
                tier.map(PlanTier::as_str),
            );
            changes.tier = Some(tier);
        }
        Ok((changes, audited))
    }
}
