//! Settling a planned item for one month (spec §8.9 R3, R9a): the occurrences of a recurring
//! bill or a one-off in that month were paid some way the forecast can't see, or won't
//! happen. It never changes the item, and the forecast and the report read it.
//!
//! The same rules as any other plan write: `all` at `write`, decided before anything is
//! looked up, so a token without it can't tell a hidden item from a missing one. A write
//! that says what the month already says writes nothing.

use std::collections::BTreeMap;

use coffer_domain::audit::{AuditChanges, AuditValue};
use coffer_domain::identity::{Actor, Via};
use coffer_domain::ids::{AgentTokenId, MemberId, PlannedItemId};
use coffer_domain::plan::{
    CalendarMonth, MAX_ITEM_SETTLEMENTS, SettlementKind, occurrences_in, settlement_applies,
    validate_as_of, validate_settlement_month,
};
use coffer_store::SettlementRecord;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::plan_items::invalid;
use super::{Ledger, LedgerError, audit};
use crate::http::problem::ProblemCode;

const ENTITY: &str = "planned_item";
const NOT_SETTLEABLE: &str = "only a recurring bill or a one-off outflow can be settled";
const NO_OCCURRENCE: &str = "the item has no occurrence in that month";
const TOO_MANY: &str = "an item holds at most 60 settlements";
const LIST_TEXT_FAILED: &str = "settlement list serialization failed";

/// One month a planned item is settled for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SettlementView {
    pub month: CalendarMonth,
    pub kind: SettlementKind,
    /// Who settled it.
    pub member: MemberId,
    /// The agent token the member acted through; `null` when signed in.
    #[schema(required)]
    pub agent: Option<AgentTokenId>,
    pub settled_at: Timestamp,
}

impl SettlementView {
    pub(super) fn new(record: &SettlementRecord) -> Self {
        Self {
            month: record.month,
            kind: record.kind,
            member: record.member,
            agent: record.agent,
            settled_at: record.settled_at,
        }
    }
}

/// How a month is settled.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettlementInput {
    /// `paid` for money gone some way the forecast can't see, `skipped` for money that
    /// won't go. The forecast treats them alike.
    pub kind: SettlementKind,
}

/// `settlements` by item and month.
pub(super) fn settled_months(
    settlements: &[SettlementRecord],
) -> BTreeMap<PlannedItemId, BTreeMap<CalendarMonth, SettlementKind>> {
    let mut found: BTreeMap<PlannedItemId, BTreeMap<CalendarMonth, SettlementKind>> =
        BTreeMap::new();
    for one in settlements {
        found
            .entry(one.item)
            .or_default()
            .insert(one.month, one.kind);
    }
    found
}

/// `settlements` as an audited value: their JSON array of month and kind.
pub(super) fn settlements_value(
    settlements: &[SettlementRecord],
) -> Result<AuditValue, LedgerError> {
    let pairs: Vec<(CalendarMonth, SettlementKind)> = settlements
        .iter()
        .map(|one| (one.month, one.kind))
        .collect();
    serde_json::to_string(&pairs)
        .map(AuditValue::Text)
        .map_err(|_| LedgerError::Internal(LIST_TEXT_FAILED))
}

impl Ledger {
    /// Settles item `id` for `month`. Saying what the month already says writes nothing and
    /// answers with what is there, who and when included.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows it, whether or
    /// not the item exists; [`LedgerError::Invalid`] for a month outside the plan's years or
    /// too far from today, an item that can't be settled, or a month holding no occurrence
    /// of it; [`LedgerError::NotFound`] for an unknown item; [`LedgerError::Conflict`] when
    /// the item already holds [`MAX_ITEM_SETTLEMENTS`]; storage failures.
    pub async fn settle_planned_item(
        &self,
        actor: &Actor,
        id: PlannedItemId,
        month: CalendarMonth,
        kind: SettlementKind,
    ) -> Result<SettlementView, LedgerError> {
        self.authorize_plan_write(actor).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let today = self.time.today_seen_by(&mut tx).await?;
        validate_settlement_month(month, today).map_err(invalid)?;
        let item = tx.get_planned_item(id).await?;
        if !settlement_applies(item.direction, &item.schedule, item.spending) {
            return Err(LedgerError::Invalid(NOT_SETTLEABLE));
        }
        if occurrences_in(&item.schedule, month).is_empty() {
            return Err(LedgerError::Invalid(NO_OCCURRENCE));
        }
        // Inside the write lock, so concurrent writes can't pass the limit.
        let settlements = tx.settlements_of_item(id).await?;
        let month_text = month.to_string();
        let audited = match settlements.iter().find(|one| one.month == month) {
            Some(same) if same.kind == kind => return Ok(SettlementView::new(same)),
            Some(other) => AuditChanges::new()
                .change("month", month_text.as_str(), month_text.as_str())
                .change("kind", other.kind.as_str(), kind.as_str()),
            None if settlements.len() >= MAX_ITEM_SETTLEMENTS => {
                return Err(LedgerError::Conflict(ProblemCode::Conflict, TOO_MANY));
            }
            None => AuditChanges::new()
                .set("month", month_text)
                .set("kind", kind.as_str()),
        };
        let record = SettlementRecord {
            item: id,
            month,
            kind,
            member: actor.member,
            agent: match actor.via {
                Via::Session => None,
                Via::Agent { token, .. } => Some(token),
            },
            settled_at: now,
        };
        tx.put_settlement(&record).await?;
        audit(
            &mut tx,
            now,
            actor,
            "planned_item.settle",
            ENTITY,
            id,
            audited,
        )
        .await?;
        tx.commit().await?;
        Ok(SettlementView::new(&record))
    }

    /// Removes item `id`'s settlement for `month`, whose occurrences then count again. A
    /// month with none answers the same and writes nothing. It may be any month of the
    /// plan's years, so one that has aged out of the window can still make room.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] unless [`Self::authorize_plan_write`] allows it, whether or
    /// not the item exists; [`LedgerError::Invalid`] for a month outside the plan's years;
    /// [`LedgerError::NotFound`] for an unknown item; storage failures.
    pub async fn unsettle_planned_item(
        &self,
        actor: &Actor,
        id: PlannedItemId,
        month: CalendarMonth,
    ) -> Result<(), LedgerError> {
        self.authorize_plan_write(actor).await?;
        validate_as_of(month.first_day()).map_err(invalid)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.get_planned_item(id).await?;
        let settlements = tx.settlements_of_item(id).await?;
        let Some(current) = settlements.iter().find(|one| one.month == month) else {
            return Ok(());
        };
        tx.delete_settlement(id, month).await?;
        audit(
            &mut tx,
            now,
            actor,
            "planned_item.unsettle",
            ENTITY,
            id,
            AuditChanges::new()
                .unset("month", month.to_string())
                .unset("kind", current.kind.as_str()),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
