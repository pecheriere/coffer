//! Planned items (spec §6.7, §8.9): the recurring bills, one-off expenses,
//! sinking funds and expected income of the household's plan.
//!
//! An item hinting at an account the actor can't see at `transactions`, and
//! any item for an actor holding `transactions` on no account, is treated as
//! missing, because a refusal would confirm it exists (R3). Changes need
//! `all` at `write`, like a category: the plan is shared by every account.

use std::collections::BTreeMap;

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::Money;
use coffer_domain::plan::{
    CalendarMonth, EffectiveFrom, InvalidMonth, MAX_PLAN_ITEMS, MonthSpan, PlanDirection,
    PlanError, PlanSchedule, PlanSpending, PlanTier, PlannedItemName, resolved_tier,
    spending_applies, validate_amount,
};
use coffer_domain::policy::{Action, authorize, plan_item_visible};
use coffer_domain::transactions::{CategoryKind, Notes};
use coffer_store::{
    AccountRecord, CategoryRecord, NewPlannedItem, PlannedItemRecord, SettlementRecord, StoreError,
    StoreTx,
};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use self::patch::{CheckedPatch, Planned};
use super::settlements::{SettlementView, settlements_value};
use super::transactions::{Patch, notes};
use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;
use crate::http::problem::ProblemCode;

mod patch;

const ENTITY: &str = "planned_item";
const HINT_INVALID: &str = "the account hint names no open account";
pub(super) const UNKNOWN_CATEGORY: &str = "the category does not exist";
const ARCHIVED_CATEGORY: &str = "the category is archived";
const OUTFLOW_CATEGORY: &str = "an outflow needs an expense category";
const INFLOW_CATEGORY: &str = "an inflow needs an income category";
const TYPE_CHANGED: &str = "a planned item's type never changes";
const SPENDING_REFUSED: &str = "only a recurring outflow has a spending kind";
const TOO_MANY: &str = "a household holds at most 500 planned items";
const GOAL_LINKED: &str = "the goal has linked splits; mark it complete instead";
const GOAL_LINKED_CURRENCY: &str = "the goal has linked splits, which hold it to its currency";
const SCHEDULE_TEXT_FAILED: &str = "schedule serialization failed";
const NO_MONTH: &str = "today has no calendar month";
const SETTLED: &str = "the item is settled for some months: delete those settlements first";

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
    /// A recurring outflow's kind; `null` on anything else. A `budget` counts
    /// all of this month's occurrences, those already past too, less what was
    /// spent in its category so far this month; a `bill` counts in full.
    #[schema(required)]
    pub spending: Option<PlanSpending>,
    /// Which tier an outflow's money is set in, which pages call its bucket; `null` for
    /// income. A goal is always `saving_toward`, and nothing else is.
    #[schema(required)]
    pub tier: Option<PlanTier>,
    /// The months it is settled for, ordered by month: its occurrences in them count as
    /// nothing in a forecast.
    pub settlements: Vec<SettlementView>,
    /// Its history: each version of `category`, `amount`, `direction`, `spending` and `tier`
    /// that is ever in force, oldest first, with the months it is. The values above are the
    /// one in force this month. A version replaced before it began is not listed.
    pub versions: Vec<PlanVersionView>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// One version of a planned item's values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlanVersionView {
    /// The first month it is in force; `null` on the first version, which also reads for
    /// every month before it.
    #[schema(required)]
    pub from: Option<CalendarMonth>,
    /// The first month it is no longer in force; `null` while it is the latest.
    #[schema(required)]
    pub until: Option<CalendarMonth>,
    pub category: CategoryId,
    pub amount: Money,
    pub direction: PlanDirection,
    #[schema(required)]
    pub spending: Option<PlanSpending>,
    #[schema(required)]
    pub tier: Option<PlanTier>,
    /// The day it was written.
    #[schema(value_type = String, format = Date)]
    pub written_on: Date,
}

impl PlannedItemView {
    fn new(record: &PlannedItemRecord, settlements: Vec<SettlementView>) -> Self {
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
            tier: record.tier,
            settlements,
            versions: record
                .versions
                .timeline()
                .into_iter()
                .map(|one| {
                    let MonthSpan { from, until } = one.span;
                    let values = one.version.values;
                    PlanVersionView {
                        from,
                        until,
                        category: values.category,
                        amount: values.amount,
                        direction: values.direction,
                        spending: values.spending,
                        tier: values.tier,
                        written_on: one.version.written_on,
                    }
                })
                .collect(),
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
    /// counts all of this month's occurrences, those already past too, less
    /// what was spent in its category so far this month; a `bill` counts in
    /// full.
    pub spending: Option<PlanSpending>,
    /// The tier of an outflow, which pages call its bucket: `must_pay`, `essentials` or
    /// `flexible`, and `saving_toward` on a goal, which can be nothing else. Income has none.
    /// By default a bill and a one-off are `must_pay`, a budget is `flexible` and a goal
    /// `saving_toward`.
    pub tier: Option<PlanTier>,
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
    /// Only on an item left an outflow: `saving_toward` on a goal and nothing else, and
    /// `must_pay`, `essentials` or `flexible` on anything else. Absent, an outflow keeps its
    /// tier, even from bill to budget; an item that becomes income loses it, and one that
    /// becomes an outflow takes its kind's default.
    pub tier: Option<PlanTier>,
    /// From when `category`, `amount`, `direction`, `spending` and `tier` change: `this_month`
    /// (the default), `next_month` (only on a recurring item), or a month written `YYYY-MM`
    /// that is this month or next. A past month's plan never changes, so any other month is
    /// refused, and so is a change to a one-off dated in a past month. A bill already paid
    /// stays paid when its amount changes. Ignored when none of those five changes.
    pub effective_from: Option<EffectiveFrom>,
}

fn views_of(settlements: &[SettlementRecord]) -> Vec<SettlementView> {
    settlements.iter().map(SettlementView::new).collect()
}

/// A day with no month: before year 0, which the household's day never is.
fn invalid_month(_: InvalidMonth) -> LedgerError {
    LedgerError::Internal(NO_MONTH)
}

/// The message for a rejected plan value; never echoes the input.
pub(super) fn invalid(error: PlanError) -> LedgerError {
    LedgerError::Invalid(match error {
        PlanError::InvalidName => "name must be 1 to 100 characters without control characters",
        PlanError::InvalidAmount => "amount must be between 1 and 1000000000000000 minor units",
        PlanError::InvalidEvery => "every must be 1 to 999",
        PlanError::DateOutOfRange => "dates must fall in the years 1970 to 2200",
        PlanError::EndBeforeAnchor => "the end date cannot be before the anchor date",
        PlanError::GoalDatesInverted => "a goal's due date must be after its start date",
        PlanError::GoalMustBeOutflow => "a goal must be an outflow",
        PlanError::SettlementOutOfWindow => "a settlement must fall within 12 months of today",
        PlanError::IncomeHasNoTier => "income has no tier",
        PlanError::GoalIsSavingToward => "a goal is always saving toward",
        PlanError::OnlyGoalSavesToward => "only a goal is saving toward",
        PlanError::PastVersion => "only this month's or next month's plan can change",
        PlanError::InvalidEffectiveFrom => {
            "effective_from must be this_month, next_month or YYYY-MM"
        }
        PlanError::NextMonthNeedsRecurring => "only a recurring item can change from next month",
        PlanError::PastOneOffKeepsValues => {
            "a one-off in a past month keeps its amount, category and tier"
        }
        // `PlanError` is non-exhaustive, and no other variant arises here.
        _ => "the planned item is not valid",
    })
}

fn schedule_text(schedule: &PlanSchedule) -> Result<String, LedgerError> {
    serde_json::to_string(schedule).map_err(|_| LedgerError::Internal(SCHEDULE_TEXT_FAILED))
}

/// The kind an item ends up with: what `sent` says, or `default` when it says
/// nothing. An item that can't have one must not be sent one.
pub(crate) fn resolved_spending(
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
        let tier = resolved_tier(input.tier, input.direction, &input.schedule, spending)
            .map_err(invalid)?;
        let new_notes = input.notes.as_deref().map(notes).transpose()?.flatten();
        let schedule = schedule_text(&input.schedule)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let today = self.time.today_seen_by(&mut tx).await?;
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
                tier,
                written_on: today,
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
            .set("spending", record.spending.map(PlanSpending::as_str))
            .set("tier", record.tier.map(PlanTier::as_str));
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
        let view = PlannedItemView::new(&record, Vec::new());
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes a planned item. A patch that changes nothing writes nothing. One that changes
    /// its schedule removes its settlements from the household's current month on, and one
    /// that changes its amount or currency those from next month on (a bill settled for this
    /// month stays paid), in the same transaction and audit entry, and earlier ones stay. A change to the category, amount,
    /// direction, spending kind or tier is a new version of the item's values, effective from
    /// this month or next as `effective_from` says; one from this month also applies its fields to
    /// a change already pending for next month, as a second version.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows
    /// it, whether or not the item exists; [`LedgerError::NotFound`] for an
    /// unknown item; [`LedgerError::Invalid`] for a value the item may not
    /// hold; [`LedgerError::Conflict`] for a new currency on a goal with
    /// linked splits, or for making a settled bill a budget or income; storage
    /// failures.
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
        let today = self.time.today_seen_by(&mut tx).await?;
        let this_month = CalendarMonth::of(today).map_err(invalid_month)?;
        let current = tx.get_planned_item(id, this_month).await?;
        let Planned {
            changes,
            versions,
            mut audited,
            settlements_end_from,
        } = patch.changes(&mut tx, &current, today).await?;
        let mut settlements = tx.settlements_of_item(id).await?;
        if audited.is_empty() {
            return Ok(PlannedItemView::new(&current, views_of(&settlements)));
        }
        // A settlement was made on what the item then owed and when, so a change to either
        // ends those still to matter. Earlier months are what happened. It errs toward
        // counting, and the member can settle again.
        if let Some(from) = settlements_end_from {
            let (ended, earlier): (Vec<_>, Vec<_>) =
                settlements.into_iter().partition(|one| one.month >= from);
            for one in &ended {
                tx.delete_settlement(id, one.month).await?;
            }
            if !ended.is_empty() {
                audited = audited.unset("settlements", settlements_value(&ended)?);
            }
            settlements = earlier;
        }
        tx.update_planned_item(id, changes, now).await?;
        for (month, values) in versions {
            tx.add_plan_version(id, today, EffectiveFrom::Month(month), values)
                .await?;
        }
        let record = tx.get_planned_item(id, this_month).await?;
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
        Ok(PlannedItemView::new(&record, views_of(&settlements)))
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
        let this_month =
            CalendarMonth::of(self.time.today_seen_by(&mut tx).await?).map_err(invalid_month)?;
        let current = tx.get_planned_item(id, this_month).await?;
        if tx.goal_has_links(id).await? {
            return Err(LedgerError::Conflict(ProblemCode::Conflict, GOAL_LINKED));
        }
        let settlements = tx.settlements_of_item(id).await?;
        tx.delete_planned_item(id).await?;
        let mut changes = AuditChanges::new()
            .unset("name", current.name.as_str())
            .unset("category", current.category)
            .unset("amount_minor", current.amount.minor_units)
            .unset("currency", current.amount.currency.as_str())
            .unset("direction", current.direction.as_str())
            .unset("account_hint", current.account_hint)
            .unset("notes", current.notes.as_ref().map(Notes::as_str))
            .unset("active", current.active)
            .unset("schedule", schedule_text(&current.schedule)?)
            .unset("spending", current.spending.map(PlanSpending::as_str))
            .unset("tier", current.tier.map(PlanTier::as_str));
        if !settlements.is_empty() {
            changes = changes.unset("settlements", settlements_value(&settlements)?);
        }
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
        let this_month = CalendarMonth::of(self.today().await?).map_err(invalid_month)?;
        let record = self.store.get_planned_item(id, this_month).await?;
        if !plan_item_visible(&access, record.account_hint) {
            return Err(LedgerError::NotFound);
        }
        let settlements = self.store.settlements_of_item(id).await?;
        Ok(PlannedItemView::new(&record, views_of(&settlements)))
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
        let this_month = CalendarMonth::of(self.today().await?).map_err(invalid_month)?;
        let records = self
            .store
            .list_planned_items(include_inactive, this_month)
            .await?;
        let mut settlements: BTreeMap<PlannedItemId, Vec<SettlementView>> = BTreeMap::new();
        for one in self.store.settlements().await? {
            settlements
                .entry(one.item)
                .or_default()
                .push(SettlementView::new(&one));
        }
        Ok(records
            .iter()
            .filter(|record| plan_item_visible(&access, record.account_hint))
            .map(|record| {
                let settled = settlements.remove(&record.id).unwrap_or_default();
                PlannedItemView::new(record, settled)
            })
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
            PlanError::SettlementOutOfWindow,
            PlanError::IncomeHasNoTier,
            PlanError::GoalIsSavingToward,
            PlanError::OnlyGoalSavesToward,
            PlanError::InvalidEffectiveFrom,
        ] {
            assert_eq!(invalid(error).to_string(), error.to_string());
        }
    }
}
