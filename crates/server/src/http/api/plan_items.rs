//! `/api/v1/plan-items`: the household's planned items.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::ids::PlannedItemId;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::plan_items::{PlannedItemInput, PlannedItemPatch, PlannedItemView};

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
                   outflow. A goal and a one-off describing the same expense are both counted, \
                   so the expense counts twice: nothing de-duplicates them. Its type never \
                   changes once it exists. Needs `write`; a token with account grants also needs \
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
                   schedule, and its `type` must stay the item's own. Needs `write`; a token \
                   with account grants also needs `all` at `write`.",
    security(("bearer" = [])),
    params(PlannedItemPath),
    request_body = PlannedItemPatch,
    responses(
        (status = 200, description = "The planned item as it now is.", body = PlannedItemView),
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
    description = "Deletes a planned item; the audit log keeps what it held. Needs `write`; a \
                   token with account grants also needs `all` at `write`.",
    security(("bearer" = [])),
    params(PlannedItemPath),
    responses(
        (status = 204, description = "The planned item is deleted."),
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
