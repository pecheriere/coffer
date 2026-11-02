//! Planned items (spec §6.7, §8.9): the recurring bills, one-off expenses,
//! sinking funds and expected income of the household's plan.
//!
//! An item hinting at an account the actor can't see at `transactions`, and
//! any item for an actor holding `transactions` on no account, is treated as
//! missing, because a refusal would confirm it exists (R3). Changes need
//! `all` at `write`, like a category: the plan is shared by every account.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::Money;
use coffer_domain::plan::{
    MAX_PLAN_ITEMS, PlanDirection, PlanError, PlanSchedule, PlanSpending, PlannedItemName,
    spending_applies, validate_amount,
};
use coffer_domain::policy::{Action, authorize, plan_item_visible};
use coffer_domain::transactions::{CategoryKind, Notes};
use coffer_store::{
    AccountRecord, CategoryRecord, NewPlannedItem, PlannedItemChanges, PlannedItemRecord,
    StoreError, StoreTx,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::transactions::{Patch, notes};
use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::ProblemCode;

const ENTITY: &str = "planned_item";
const HINT_INVALID: &str = "the account hint names no open account";
const UNKNOWN_CATEGORY: &str = "the category does not exist";
const ARCHIVED_CATEGORY: &str = "the category is archived";
const OUTFLOW_CATEGORY: &str = "an outflow needs an expense category";
const INFLOW_CATEGORY: &str = "an inflow needs an income category";
const TYPE_CHANGED: &str = "a planned item's type never changes";
const SPENDING_REFUSED: &str = "only a recurring outflow has a spending kind";
const TOO_MANY: &str = "a household holds at most 500 planned items";
const GOAL_LINKED: &str = "the goal has linked splits; mark it complete instead";
const GOAL_LINKED_CURRENCY: &str = "the goal has linked splits, which hold it to its currency";
const SCHEDULE_TEXT_FAILED: &str = "schedule serialization failed";

/// A planned item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlannedItemView {
    pub id: PlannedItemId,
    pub name: String,
    pub category: CategoryId,
    /// A positive magnitude: `direction` says which way the money goes.
    pub amount: Money,
    pub direction: PlanDirection,
    /// The account it is expected on; `null` for none.
    #[schema(required)]
    pub account_hint: Option<AccountId>,
    #[schema(required)]
    pub notes: Option<String>,
    /// An inactive item stays stored, but is left out of forecasts and of
    /// lists that don't ask for it.
    pub active: bool,
    pub schedule: PlanSchedule,
    /// A recurring outflow's kind; `null` on anything else. A `budget` is
    /// reduced by what was spent in its category so far this month, a `bill`
    /// counts in full.
    #[schema(required)]
    pub spending: Option<PlanSpending>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl PlannedItemView {
    fn new(record: &PlannedItemRecord) -> Self {
        Self {
            id: record.id,
            name: record.name.as_str().to_owned(),
            category: record.category,
            amount: record.amount,
            direction: record.direction,
            account_hint: record.account_hint,
            notes: record.notes.as_ref().map(|notes| notes.as_str().to_owned()),
            active: record.active,
            schedule: record.schedule,
            spending: record.spending,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// A planned item to create.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlannedItemInput {
    /// 1 to 100 characters, without control characters.
    pub name: String,
    /// An `expense` category for an outflow, an `income` one for an inflow;
    /// not archived, nor in an archived group.
    pub category: CategoryId,
    /// A positive magnitude, 1 to 10^15 minor units: `direction` says which
    /// way the money goes.
    pub amount: Money,
    /// A goal must be an `outflow`.
    pub direction: PlanDirection,
    /// An open account it is expected on. An item with a hint is visible only
    /// to tokens holding `transactions` on that account.
    pub account_hint: Option<AccountId>,
    /// At most 2000 characters; newlines are the only control characters
    /// allowed. `""` is no notes.
    pub notes: Option<String>,
    /// Whether it counts, `true` by default.
    pub active: Option<bool>,
    /// Its type never changes once it exists.
    pub schedule: PlanSchedule,
    /// Only on a recurring outflow, where it is `bill` by default: a `budget`
    /// is reduced by what was spent in its category so far this month, a
    /// `bill` counts in full.
    pub spending: Option<PlanSpending>,
}

/// Changes to a planned item. An absent field stays as it is.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlannedItemPatch {
    /// 1 to 100 characters, without control characters.
    pub name: Option<String>,
    /// An `expense` category for an outflow, an `income` one for an inflow;
    /// not archived, nor in an archived group.
    pub category: Option<CategoryId>,
    /// A positive magnitude, 1 to 10^15 minor units. A goal with linked splits
    /// keeps its currency.
    pub amount: Option<Money>,
    /// A goal must be an `outflow`.
    pub direction: Option<PlanDirection>,
    /// An open account; `null` removes the hint.
    #[serde(default)]
    #[schema(value_type = Option<AccountId>)]
    pub account_hint: Patch<AccountId>,
    /// At most 2000 characters; newlines are the only control characters
    /// allowed. `null` or `""` clears them.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub notes: Patch<String>,
    pub active: Option<bool>,
    /// Replaces the whole schedule, whose `type` must stay the item's own: a
    /// goal's `completed` goes back to `false` unless it is sent.
    pub schedule: Option<PlanSchedule>,
    /// Only on an item left a recurring outflow. An item that stops being an
    /// outflow loses its kind; one that becomes an outflow is a `bill` unless
    /// this says otherwise.
    pub spending: Option<PlanSpending>,
}

/// The message for a rejected plan value; never echoes the input.
fn invalid(error: PlanError) -> LedgerError {
    LedgerError::Invalid(match error {
        PlanError::InvalidName => "name must be 1 to 100 characters without control characters",
        PlanError::InvalidAmount => "amount must be between 1 and 1000000000000000 minor units",
        PlanError::InvalidEvery => "every must be 1 to 999",
        PlanError::DateOutOfRange => "dates must fall in the years 1970 to 2200",
        PlanError::EndBeforeAnchor => "the end date cannot be before the anchor date",
        PlanError::GoalDatesInverted => "a goal's due date must be after its start date",
        PlanError::GoalMustBeOutflow => "a goal must be an outflow",
        // `PlanError` is non-exhaustive, and no other variant arises here.
        _ => "the planned item is not valid",
    })
}

fn schedule_text(schedule: &PlanSchedule) -> Result<String, LedgerError> {
    serde_json::to_string(schedule).map_err(|_| LedgerError::Internal(SCHEDULE_TEXT_FAILED))
}

/// The kind an item ends up with: what `sent` says, or `default` when it says
/// nothing. An item that can't have one must not be sent one.
fn resolved_spending(
    sent: Option<PlanSpending>,
    default: PlanSpending,
    direction: PlanDirection,
    schedule: &PlanSchedule,
) -> Result<Option<PlanSpending>, LedgerError> {
    if spending_applies(direction, schedule) {
        Ok(Some(sent.unwrap_or(default)))
    } else if sent.is_some() {
        Err(LedgerError::Invalid(SPENDING_REFUSED))
    } else {
        Ok(None)
    }
}

/// `Ok` when `found`, the caller's own lookup of a hinted account, is an open
/// account.
///
/// # Errors
///
/// [`LedgerError::Invalid`] for a missing or closed account; storage failures.
pub(crate) fn check_hint(found: Result<AccountRecord, StoreError>) -> Result<(), LedgerError> {
    match found {
        Ok(account) if account.closed_at.is_none() => Ok(()),
        Ok(_) | Err(StoreError::NotFound) => Err(LedgerError::Invalid(HINT_INVALID)),
        Err(other) => Err(other.into()),
    }
}

/// `Ok` when an item going in `direction` may use `category`: not archived,
/// nor in an archived group, and of the kind that direction needs. A
/// `transfer` category is neither, since a transfer isn't a need.
///
/// # Errors
///
/// [`LedgerError::Invalid`] saying which rule it broke.
pub(crate) fn check_item_category(
    category: &CategoryRecord,
    group_archived: bool,
    direction: PlanDirection,
) -> Result<(), LedgerError> {
    if category.archived_at.is_some() || group_archived {
        return Err(LedgerError::Invalid(ARCHIVED_CATEGORY));
    }
    match (direction, category.kind) {
        (PlanDirection::Outflow, CategoryKind::Expense)
        | (PlanDirection::Inflow, CategoryKind::Income) => Ok(()),
        (PlanDirection::Outflow, _) => Err(LedgerError::Invalid(OUTFLOW_CATEGORY)),
        (PlanDirection::Inflow, _) => Err(LedgerError::Invalid(INFLOW_CATEGORY)),
    }
}

/// Looks `id` up inside `tx` and checks an item going in `direction` may use
/// it.
async fn check_category(
    tx: &mut StoreTx,
    id: CategoryId,
    direction: PlanDirection,
) -> Result<(), LedgerError> {
    let category = match tx.get_category(id).await {
        Ok(category) => category,
        Err(StoreError::NotFound) => return Err(LedgerError::Invalid(UNKNOWN_CATEGORY)),
        Err(other) => return Err(other.into()),
    };
    let group = tx.get_category_group(category.group).await?;
    check_item_category(&category, group.archived_at.is_some(), direction)
}

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
struct CheckedPatch {
    name: Option<PlannedItemName>,
    amount: Option<Money>,
    notes: Patch<Notes>,
    category: Option<CategoryId>,
    direction: Option<PlanDirection>,
    account_hint: Patch<AccountId>,
    active: Option<bool>,
    schedule: Option<PlanSchedule>,
    spending: Option<PlanSpending>,
}

impl CheckedPatch {
    fn new(patch: PlannedItemPatch) -> Result<Self, LedgerError> {
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
        })
    }

    /// Checks what this patch changes against the item it would leave, and
    /// returns what differs from `current` with its audit record. What it
    /// leaves alone isn't checked again: an item whose category has since been
    /// archived can still be renamed.
    async fn changes(
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
        Ok((changes, audited))
    }
}

impl Ledger {
    /// Whether `actor` may create, change or delete planned items: the plan
    /// is shared by every account, so an `all` grant at write is needed.
    /// Decided before a create reserves its idempotency key.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`] or write
    /// access to every account; a failure to resolve access.
    pub async fn authorize_plan_write(&self, actor: &Actor) -> Result<(), LedgerError> {
        self.authorize_shared_write(actor).await
    }

    /// Creates a planned item. With `slot`, the idempotency key is completed
    /// with the response in the same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows
    /// it; [`LedgerError::Invalid`] for a value the item may not hold;
    /// [`LedgerError::Conflict`] when the household already holds
    /// [`MAX_PLAN_ITEMS`]; storage failures.
    pub async fn create_planned_item(
        &self,
        actor: &Actor,
        input: PlannedItemInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<PlannedItemView, LedgerError> {
        self.authorize_plan_write(actor).await?;
        let name = PlannedItemName::parse(&input.name).map_err(invalid)?;
        validate_amount(input.amount).map_err(invalid)?;
        input.schedule.validate(input.direction).map_err(invalid)?;
        let spending = resolved_spending(
            input.spending,
            PlanSpending::Bill,
            input.direction,
            &input.schedule,
        )?;
        let new_notes = input.notes.as_deref().map(notes).transpose()?.flatten();
        let schedule = schedule_text(&input.schedule)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        // Inside the write lock, so concurrent creates can't pass the limit.
        if tx.count_planned_items().await? >= MAX_PLAN_ITEMS {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, TOO_MANY));
        }
        check_category(&mut tx, input.category, input.direction).await?;
        if let Some(account) = input.account_hint {
            check_hint(tx.get_account(account).await)?;
        }
        let record = tx
            .insert_planned_item(NewPlannedItem {
                name,
                category: input.category,
                amount: input.amount,
                direction: input.direction,
                account_hint: input.account_hint,
                notes: new_notes,
                active: input.active.unwrap_or(true),
                schedule: input.schedule,
                spending,
                created_at: now,
            })
            .await?;
        let changes = AuditChanges::new()
            .set("name", record.name.as_str())
            .set("category", record.category)
            .set("amount_minor", record.amount.minor_units)
            .set("currency", record.amount.currency.as_str())
            .set("direction", record.direction.as_str())
            .set("account_hint", record.account_hint)
            .set("notes", record.notes.as_ref().map(Notes::as_str))
            .set("active", record.active)
            .set("schedule", schedule)
            .set("spending", record.spending.map(PlanSpending::as_str));
        audit(
            &mut tx,
            now,
            actor,
            "planned_item.create",
            ENTITY,
            record.id,
            changes,
        )
        .await?;
        let view = PlannedItemView::new(&record);
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a planned item. A patch that changes nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows
    /// it, whether or not the item exists; [`LedgerError::NotFound`] for an
    /// unknown item; [`LedgerError::Invalid`] for a value the item may not
    /// hold; [`LedgerError::Conflict`] for a new currency on a goal with
    /// linked splits; storage failures.
    pub async fn update_planned_item(
        &self,
        actor: &Actor,
        id: PlannedItemId,
        patch: PlannedItemPatch,
    ) -> Result<PlannedItemView, LedgerError> {
        self.authorize_plan_write(actor).await?;
        let patch = CheckedPatch::new(patch)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_planned_item(id).await?;
        let (changes, audited) = patch.changes(&mut tx, &current).await?;
        if audited.is_empty() {
            return Ok(PlannedItemView::new(&current));
        }
        let record = tx.update_planned_item(id, changes, now).await?;
        audit(
            &mut tx,
            now,
            actor,
            "planned_item.update",
            ENTITY,
            id,
            audited,
        )
        .await?;
        tx.commit().await?;
        Ok(PlannedItemView::new(&record))
    }

    /// Deletes a planned item. Its audit entry keeps every field as it was.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows
    /// it, whether or not the item exists; [`LedgerError::NotFound`] for an
    /// unknown item; [`LedgerError::Conflict`] for a goal with linked splits;
    /// storage failures.
    pub async fn delete_planned_item(
        &self,
        actor: &Actor,
        id: PlannedItemId,
    ) -> Result<(), LedgerError> {
        self.authorize_plan_write(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_planned_item(id).await?;
        if tx.goal_has_links(id).await? {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, GOAL_LINKED));
        }
        tx.delete_planned_item(id).await?;
        let changes = AuditChanges::new()
            .unset("name", current.name.as_str())
            .unset("category", current.category)
            .unset("amount_minor", current.amount.minor_units)
            .unset("currency", current.amount.currency.as_str())
            .unset("direction", current.direction.as_str())
            .unset("account_hint", current.account_hint)
            .unset("notes", current.notes.as_ref().map(Notes::as_str))
            .unset("active", current.active)
            .unset("schedule", schedule_text(&current.schedule)?)
            .unset("spending", current.spending.map(PlanSpending::as_str));
        audit(
            &mut tx,
            now,
            actor,
            "planned_item.delete",
            ENTITY,
            id,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// One planned item.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown item and for one the actor
    /// may not see, alike; [`LedgerError::Denied`] without
    /// [`Action::ReadHousehold`]; storage failures.
    pub async fn get_planned_item(
        &self,
        actor: &Actor,
        id: PlannedItemId,
    ) -> Result<PlannedItemView, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let record = self.store.get_planned_item(id).await?;
        if plan_item_visible(&access, record.account_hint) {
            Ok(PlannedItemView::new(&record))
        } else {
            Err(LedgerError::NotFound)
        }
    }

    /// The planned items the actor may see, ordered by id; inactive ones only
    /// when `include_inactive`. Holding `transactions` on no account leaves
    /// the list empty, not refused.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage
    /// failures.
    pub async fn list_planned_items(
        &self,
        actor: &Actor,
        include_inactive: bool,
    ) -> Result<Vec<PlannedItemView>, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let records = self.store.list_planned_items(include_inactive).await?;
        Ok(records
            .iter()
            .filter(|record| plan_item_visible(&access, record.account_hint))
            .map(PlannedItemView::new)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::plan::PlanError;

    use super::invalid;

    /// The static messages must say what the domain's errors say.
    #[test]
    fn messages_match_the_domains_errors() {
        for error in [
            PlanError::InvalidName,
            PlanError::InvalidAmount,
            PlanError::InvalidEvery,
            PlanError::DateOutOfRange,
            PlanError::EndBeforeAnchor,
            PlanError::GoalDatesInverted,
            PlanError::GoalMustBeOutflow,
        ] {
            assert_eq!(invalid(error).to_string(), error.to_string());
        }
    }
}
