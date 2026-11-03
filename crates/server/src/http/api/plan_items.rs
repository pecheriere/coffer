//! `/api/v1/plan-items`: the household's planned items.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::plan::CalendarMonth;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::plan_items::{PlannedItemInput, PlannedItemPatch, PlannedItemView};
use crate::ledger::settlements::{SettlementInput, SettlementView};

/// The planned items the token may see. At most 500 exist, so a list has no
/// cursor.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannedItemList {
    pub items: Vec<PlannedItemView>,
}

/// Whether to list inactive items.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PlannedItemListQuery {
    /// Also list inactive items (default `false`).
    pub include_inactive: Option<bool>,
}

/// One planned item.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct PlannedItemPath {
    /// The planned item's id.
    pub item_id: PlannedItemId,
}

/// One month of a planned item.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct SettlementPath {
    /// The planned item's id.
    pub item_id: PlannedItemId,
    /// The month, written `YYYY-MM`.
    pub month: CalendarMonth,
}

/// Every planned item the token may see.
#[utoipa::path(
    get,
    path = "/api/v1/plan-items",
    operation_id = "listPlanItems",
    tag = tags::PLAN,
    description = "The planned items, ordered by id. Inactive items are listed only with \
                   `include_inactive=true`. Needs `read`. A token with account grants sees an \
                   item with an account hint only if it holds `transactions` on that account, \
                   and one without a hint only if it holds `transactions` on some account: a \
                   token holding only `balances` gets an empty list. A goal and a one-off \
                   describing the same expense are both counted; nothing de-duplicates them.",
    security(("bearer" = [])),
    params(PlannedItemListQuery),
    responses(
        (status = 200, description = "The planned items.", body = PlannedItemList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<PlannedItemListQuery>,
) -> Result<Json<PlannedItemList>, Problem> {
    let items = state
        .ledger
        .list_planned_items(&actor, query.include_inactive.unwrap_or(false))
        .await?;
    Ok(Json(PlannedItemList { items }))
}

/// Creates a planned item.
#[utoipa::path(
    post,
    path = "/api/v1/plan-items",
    operation_id = "createPlanItem",
    tag = tags::PLAN,
    description = "Creates a recurring item, a one-off or a goal (a sinking fund). An outflow \
                   needs an `expense` category and an inflow an `income` one; a goal must be an \
                   outflow. A recurring outflow is a `bill`, counted in full, unless `spending` \
                   makes it a `budget`, which counts all of this month's occurrences, those \
                   already past too, less what was spent in its category so far this month; \
                   nothing else takes `spending`. A goal and a one-off describing the \
                   same expense are both counted, so the expense counts twice: nothing \
                   de-duplicates them. Its type never changes once it exists. Needs `write`; a token with account grants also needs \
                   `all` at `write`. With an `Idempotency-Key`, a retry of the same request \
                   within 24 hours returns the first response instead of creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        token (or session) for 24 hours."),
    ),
    request_body = PlannedItemInput,
    responses(
        (status = 201, description = "The planned item, created now or by an earlier request \
                                      with the same `Idempotency-Key`.", body = PlannedItemView),
        (status = 409, description = "The household already holds 500 planned items, or a \
                                      request with this `Idempotency-Key` is still in progress.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn create(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    method: Method,
    uri: Uri,
    key: Option<IdempotencyKey>,
    TypedJsonBytes { value, bytes }: TypedJsonBytes<PlannedItemInput>,
) -> Result<Response, Problem> {
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || Ok(state.ledger.authorize_plan_write(&actor).await?);
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state.ledger.create_planned_item(&actor, value, slot).await
    })
    .await
}

/// One planned item.
#[utoipa::path(
    get,
    path = "/api/v1/plan-items/{item_id}",
    operation_id = "getPlanItem",
    tag = tags::PLAN,
    description = "One planned item. Needs `read`. An item the token may not see (see \
                   `listPlanItems`) answers exactly like one that does not exist.",
    security(("bearer" = [])),
    params(PlannedItemPath),
    responses(
        (status = 200, description = "The planned item.", body = PlannedItemView),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<PlannedItemPath>,
) -> Result<Json<PlannedItemView>, Problem> {
    Ok(Json(
        state.ledger.get_planned_item(&actor, path.item_id).await?,
    ))
}

/// Changes a planned item.
#[utoipa::path(
    patch,
    path = "/api/v1/plan-items/{item_id}",
    operation_id = "updatePlanItem",
    tag = tags::PLAN,
    description = "Changes the fields given and checks them against the item they would leave. \
                   `null` clears `account_hint` and `notes`. `schedule` replaces the whole \
                   schedule, and its `type` must stay the item's own. `spending` is refused unless \
                   the item is left a recurring outflow. Changing `direction` to `inflow` drops \
                   its kind; changing it to `outflow` makes it a `bill` unless `spending` says \
                   otherwise. Changing `amount`, its currency included, or `schedule` removes the \
                   item's settlements from the household's current month on, since nobody said \
                   that was paid: the answer shows what is left, the audit entry says which went, \
                   and earlier months stay. Any other change keeps them, and a settled item can't \
                   become a budget or income until they are deleted. Needs `write`; a token with \
                   account grants also needs `all` at `write`.",
    security(("bearer" = [])),
    params(PlannedItemPath),
    request_body = PlannedItemPatch,
    responses(
        (status = 200, description = "The planned item as it now is.", body = PlannedItemView),
        (status = 409, description = "A new currency on a goal with linked splits, or a settled \
                                      item becoming a budget or income.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<PlannedItemPath>,
    TypedJson(changes): TypedJson<PlannedItemPatch>,
) -> Result<Json<PlannedItemView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_planned_item(&actor, path.item_id, changes)
            .await?,
    ))
}

/// Deletes a planned item.
#[utoipa::path(
    delete,
    path = "/api/v1/plan-items/{item_id}",
    operation_id = "deletePlanItem",
    tag = tags::PLAN,
    description = "Deletes a planned item; the audit log keeps what it held. A goal with linked \
                   splits can't be deleted: mark it complete instead. Needs `write`; a token \
                   with account grants also needs `all` at `write`.",
    security(("bearer" = [])),
    params(PlannedItemPath),
    responses(
        (status = 204, description = "The planned item is deleted."),
        (status = 409, description = "The goal has linked splits.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<PlannedItemPath>,
) -> Result<StatusCode, Problem> {
    state
        .ledger
        .delete_planned_item(&actor, path.item_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Settles a planned item for one month.
#[utoipa::path(
    put,
    path = "/api/v1/plan-items/{item_id}/settlements/{month}",
    operation_id = "settlePlanItem",
    tag = tags::PLAN,
    description = "Records that a recurring bill's or a one-off's occurrences in this month were \
                   `paid` some way the forecast can't see, or are `skipped`. In a forecast they \
                   count as nothing: no gross, no gap, no `bill_not_seen`, and `paid` keeps what it \
                   was paid with off the category's budgets. It never changes the item. The forecast lists them in \
                   `settled`; plan versus actual keeps them in `planned` and says they are settled. \
                   The month must be in the years 1970 to 2200, at most 12 months from today's \
                   either way, and hold an occurrence of the item. A budget, a goal and income can't \
                   be settled. Sending what the month already says writes nothing and answers with \
                   it, who and when included. Another `kind` replaces it. Needs `write`; a token \
                   with account grants also needs `all` at `write`, decided before anything is \
                   looked up.",
    security(("bearer" = [])),
    params(SettlementPath),
    request_body = SettlementInput,
    responses(
        (status = 200, description = "The month's settlement as it now is.", body = SettlementView),
        (status = 409, description = "The item already holds 60 settlements.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn settle(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<SettlementPath>,
    TypedJson(input): TypedJson<SettlementInput>,
) -> Result<Json<SettlementView>, Problem> {
    Ok(Json(
        state
            .ledger
            .settle_planned_item(&actor, path.item_id, path.month, input.kind)
            .await?,
    ))
}

/// Removes a planned item's settlement for one month.
#[utoipa::path(
    delete,
    path = "/api/v1/plan-items/{item_id}/settlements/{month}",
    operation_id = "unsettlePlanItem",
    tag = tags::PLAN,
    description = "The month's occurrences count again. A month with no settlement answers the \
                   same and writes nothing. Any month in the years 1970 to 2200 may be cleared, \
                   however far from today, so an item at its limit can make room. Needs `write`; \
                   a token with account grants also needs `all` at `write`, decided before \
                   anything is looked up.",
    security(("bearer" = [])),
    params(SettlementPath),
    responses(
        (status = 204, description = "The month has no settlement."),
        CommonProblems,
    ),
)]
pub(super) async fn unsettle(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<SettlementPath>,
) -> Result<StatusCode, Problem> {
    state
        .ledger
        .unsettle_planned_item(&actor, path.item_id, path.month)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
