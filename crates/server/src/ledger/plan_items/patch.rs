//! A patch to a planned item: its values checked before the write lock is taken, then
//! against the item it would leave, giving what changes and its audit record.

use coffer_domain::audit::AuditChanges;
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::money::Money;
use coffer_domain::plan::{
    CalendarMonth, EffectiveFrom, PlanDirection, PlanError, PlanSchedule, PlanSpending, PlanTier,
    PlanValues, PlannedItemName, resolved_tier, settlement_applies, validate_amount,
};
use coffer_domain::transactions::Notes;
use coffer_store::{PlannedItemChanges, PlannedItemRecord, StoreTx};
use jiff::civil::Date;

use super::{
    GOAL_LINKED_CURRENCY, NO_MONTH, PlannedItemPatch, SETTLED, TYPE_CHANGED, check_category,
    check_hint, invalid, resolved_spending, schedule_text,
};
use crate::http::problem::ProblemCode;
use crate::ledger::LedgerError;
use crate::ledger::transactions::{Patch, notes};

/// `audited` with what differs between the amounts `from` and `to`.
fn amount_audited(mut audited: AuditChanges, from: Money, to: Money, prefix: &str) -> AuditChanges {
    if to.minor_units != from.minor_units {
        audited = audited.change(
            &format!("{prefix}amount_minor"),
            from.minor_units,
            to.minor_units,
        );
    }
    if to.currency != from.currency {
        audited = audited.change(
            &format!("{prefix}currency"),
            from.currency.as_str(),
            to.currency.as_str(),
        );
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
    effective_from: EffectiveFrom,
}

/// What a patch comes to: the item's own changes, the new version of its values if they
/// change, and what the audit trail says.
pub(super) struct Planned {
    pub(super) changes: PlannedItemChanges,
    /// The versions to write, in order: each with the month it takes effect from and its
    /// values. A change from this month is followed by one for next month when a change is
    /// already pending there, as the later write is the one that reads.
    pub(super) versions: Vec<(CalendarMonth, PlanValues)>,
    pub(super) audited: AuditChanges,
    /// Settlements from this month on end: the schedule changed; from next month on: the
    /// amount did.
    pub(super) settlements_end_from: Option<CalendarMonth>,
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
            effective_from: patch.effective_from.unwrap_or_default(),
        })
    }

    /// Checks what this patch changes against the item it would leave, `today` being the
    /// household's day, and returns what differs from `current` (read in `today`'s month) with
    /// its audit record. What it leaves alone isn't checked again: an item whose category
    /// has since been archived can still be renamed.
    pub(super) async fn changes(
        self,
        tx: &mut StoreTx,
        current: &PlannedItemRecord,
        today: Date,
    ) -> Result<Planned, LedgerError> {
        let this_month = CalendarMonth::of(today).map_err(|_| LedgerError::Internal(NO_MONTH))?;
        let item_type = current.schedule.item_type();
        // The values are changed from the month `effective_from` names, so they are compared
        // with the version in force then. A month that can't be written only matters if the
        // values do change.
        let effective = self.effective_from.resolve(today, item_type);
        let base = *current
            .versions
            .in_force(effective.as_ref().copied().unwrap_or(this_month));
        let direction = self.direction.unwrap_or(base.direction);
        let category = self.category.unwrap_or(base.category);
        let schedule = self.schedule.unwrap_or(current.schedule);
        if schedule.item_type() != item_type {
            return Err(LedgerError::Invalid(TYPE_CHANGED));
        }
        let direction_changed = direction != base.direction;
        if direction_changed || schedule != current.schedule {
            schedule.validate(direction).map_err(invalid)?;
        }
        if direction_changed || category != base.category {
            check_category(tx, category, direction).await?;
        }

        let spending = resolved_spending(
            self.spending,
            base.spending.unwrap_or(PlanSpending::Bill),
            direction,
            &schedule,
        )?;
        let tier = tier_left(self.tier, &base, direction, &schedule, spending)?;
        // A settlement says only what a bill or a one-off can have said of it.
        if !settlement_applies(direction, &schedule, spending)
            && settlement_applies(base.direction, &current.schedule, base.spending)
            && !tx.settlements_of_item(current.id).await?.is_empty()
        {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, SETTLED));
        }
        let amount = self.amount.unwrap_or(base.amount);
        // A link promises the goal's currency is the transaction's.
        if amount.currency != base.amount.currency && tx.goal_has_links(current.id).await? {
            return Err(LedgerError::Conflict(
                ProblemCode::Conflict,
                GOAL_LINKED_CURRENCY,
            ));
        }

        let (changes, mut audited) = self.own_changes(tx, current, &schedule).await?;

        let values = PlanValues {
            amount,
            direction,
            category,
            spending,
            tier,
        };
        let mut versions = Vec::new();
        if values != base {
            let month = effective.map_err(invalid)?;
            if let PlanSchedule::OneOff { on } = schedule
                && on < this_month.first_day()
            {
                return Err(invalid(PlanError::PastOneOffKeepsValues));
            }
            audited = values_audited(audited, &base, &values, "")
                .set("effective_from", month.to_string());
            versions.push((month, values));
            if month == this_month
                && let Some((next, carried, pending)) = self
                    .carried_to_next_month(tx, current, this_month, &schedule, &values)
                    .await?
            {
                audited = values_audited(audited, &pending, &carried, "next_month_")
                    .set("next_month_effective_from", next.to_string());
                versions.push((next, carried));
            }
        }
        let settlements_end_from = if changes.schedule.is_some() {
            Some(this_month)
        } else if values.amount != base.amount {
            // A change from this month leaves the month's own settlement: a paid bill stays
            // paid. What it settles in later months was on the amount they held.
            Some(this_month.next())
        } else {
            None
        };
        Ok(Planned {
            changes,
            versions,
            audited,
            settlements_end_from,
        })
    }
}

impl CheckedPatch {
    /// What a patch from this month leaves a change already pending for next month with: its
    /// values with the fields the patch names, unless that is what next month reads anyway (`new`
    /// being what this month's version gives it, as its sequence is higher than the pending one's).
    /// Equal to the pending values still writes: the new row re-asserts them over `new`.
    async fn carried_to_next_month(
        &self,
        tx: &mut StoreTx,
        current: &PlannedItemRecord,
        this_month: CalendarMonth,
        schedule: &PlanSchedule,
        new: &PlanValues,
    ) -> Result<Option<(CalendarMonth, PlanValues, PlanValues)>, LedgerError> {
        let next = this_month.next();
        let Some(pending) = current.versions.starting_in(next).map(|one| one.values) else {
            return Ok(None);
        };
        let direction = self.direction.unwrap_or(pending.direction);
        let category = self.category.unwrap_or(pending.category);
        schedule.validate(direction).map_err(invalid)?;
        if direction != pending.direction || category != pending.category {
            check_category(tx, category, direction).await?;
        }
        let spending = resolved_spending(
            self.spending,
            pending.spending.unwrap_or(PlanSpending::Bill),
            direction,
            schedule,
        )?;
        let tier = tier_left(self.tier, &pending, direction, schedule, spending)?;
        if !settlement_applies(direction, schedule, spending)
            && settlement_applies(pending.direction, schedule, pending.spending)
            && !tx.settlements_of_item(current.id).await?.is_empty()
        {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, SETTLED));
        }
        let carried = PlanValues {
            amount: self.amount.unwrap_or(pending.amount),
            direction,
            category,
            spending,
            tier,
        };
        Ok((carried != *new).then_some((next, carried, pending)))
    }

    /// What the patch changes of the item itself, apart from the values that have versions:
    /// its name, hint, notes, whether it is active and its `schedule`.
    async fn own_changes(
        &self,
        tx: &mut StoreTx,
        current: &PlannedItemRecord,
        schedule: &PlanSchedule,
    ) -> Result<(PlannedItemChanges, AuditChanges), LedgerError> {
        let mut changes = PlannedItemChanges::default();
        let mut audited = AuditChanges::new();
        if let Some(name) = self.name.clone().filter(|name| *name != current.name) {
            audited = audited.change("name", current.name.as_str(), name.as_str());
            changes.name = Some(name);
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
        let new_notes = match self.notes.clone() {
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
        if *schedule != current.schedule {
            audited = audited.change(
                "schedule",
                schedule_text(&current.schedule)?,
                schedule_text(schedule)?,
            );
            changes.schedule = Some(*schedule);
        }
        Ok((changes, audited))
    }
}

/// `audited` with what differs between the values `from` and `to`, each field's name led
/// by `prefix`.
fn values_audited(
    mut audited: AuditChanges,
    from: &PlanValues,
    to: &PlanValues,
    prefix: &str,
) -> AuditChanges {
    if to.category != from.category {
        audited = audited.change(&format!("{prefix}category"), from.category, to.category);
    }
    audited = amount_audited(audited, from.amount, to.amount, prefix);
    if to.direction != from.direction {
        audited = audited.change(
            &format!("{prefix}direction"),
            from.direction.as_str(),
            to.direction.as_str(),
        );
    }
    if to.spending != from.spending {
        audited = audited.change(
            &format!("{prefix}spending"),
            from.spending.map(PlanSpending::as_str),
            to.spending.map(PlanSpending::as_str),
        );
    }
    if to.tier != from.tier {
        audited = audited.change(
            &format!("{prefix}tier"),
            from.tier.map(PlanTier::as_str),
            to.tier.map(PlanTier::as_str),
        );
    }
    audited
}

/// The tier an item is left with. One staying an outflow keeps its tier unless one is sent;
/// one becoming an outflow takes its kind's default unless one is sent, and one becoming
/// income loses it.
fn tier_left(
    sent: Option<PlanTier>,
    base: &PlanValues,
    direction: PlanDirection,
    schedule: &PlanSchedule,
    spending: Option<PlanSpending>,
) -> Result<Option<PlanTier>, LedgerError> {
    let kept = if direction == base.direction {
        base.tier
    } else {
        None
    };
    resolved_tier(sent.or(kept), direction, schedule, spending).map_err(invalid)
}
