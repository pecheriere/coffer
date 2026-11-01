//! `/api/v1/categories` and `/api/v1/category-groups`: the household's
//! categories.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::Response;
use coffer_domain::ids::{CategoryGroupId, CategoryId};
use serde::Deserialize;
use utoipa::IntoParams;

use super::extract::{TypedJson, TypedJsonBytes, TypedPath};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::categories::{
    CategoryGroupInput, CategoryGroupPatch, CategoryGroupView, CategoryInput, CategoryPatch,
    CategoryTree, CategoryView,
};

/// One category group.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct CategoryGroupPath {
    /// The category group's id.
    pub group_id: CategoryGroupId,
}

/// One category.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct CategoryPath {
    /// The category's id.
    pub category_id: CategoryId,
}

/// Every category.
#[utoipa::path(
    get,
    path = "/api/v1/categories",
    operation_id = "listCategories",
    tag = tags::CATEGORIES,
    description = "Every category group with its categories, each in position order (ties \
                   oldest first), archived ones included. Needs `read`.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The categories.", body = CategoryTree),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<CategoryTree>, Problem> {
    Ok(Json(state.ledger.list_categories(&actor).await?))
}

/// Creates a category.
#[utoipa::path(
    post,
    path = "/api/v1/categories",
    operation_id = "createCategory",
    tag = tags::CATEGORIES,
    description = "Creates a category in an existing group. Needs `write`. With an \
                   `Idempotency-Key`, a retry of the same request within 24 hours returns the \
                   first response instead of creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        member for 24 hours."),
    ),
    request_body = CategoryInput,
    responses(
        (status = 201, description = "The category, created now or by an earlier request \
                                      with the same `Idempotency-Key`.", body = CategoryView),
        (status = 409, description = "A request with this `Idempotency-Key` is still in \
                                      progress.",
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
    TypedJsonBytes { value, bytes }: TypedJsonBytes<CategoryInput>,
) -> Result<Response, Problem> {
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || idempotency::write_household(&actor);
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state.ledger.create_category(&actor, value, slot).await
    })
    .await
}

/// Changes a category.
#[utoipa::path(
    patch,
    path = "/api/v1/categories/{category_id}",
    operation_id = "updateCategory",
    tag = tags::CATEGORIES,
    description = "Changes the fields given: name, kind, position, group (moving it) or \
                   archived. An archived category takes no new splits; its existing splits \
                   stay. Needs `write`.",
    security(("bearer" = [])),
    params(CategoryPath),
    request_body = CategoryPatch,
    responses(
        (status = 200, description = "The category as it now is.", body = CategoryView),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<CategoryPath>,
    TypedJson(changes): TypedJson<CategoryPatch>,
) -> Result<Json<CategoryView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_category(&actor, path.category_id, changes)
            .await?,
    ))
}

/// Creates a category group.
#[utoipa::path(
    post,
    path = "/api/v1/category-groups",
    operation_id = "createCategoryGroup",
    tag = tags::CATEGORIES,
    description = "Creates a category group. Needs `write`. With an `Idempotency-Key`, a \
                   retry of the same request within 24 hours returns the first response \
                   instead of creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        member for 24 hours."),
    ),
    request_body = CategoryGroupInput,
    responses(
        (status = 201, description = "The group, created now or by an earlier request with \
                                      the same `Idempotency-Key`.", body = CategoryGroupView),
        (status = 409, description = "A request with this `Idempotency-Key` is still in \
                                      progress.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn create_group(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    method: Method,
    uri: Uri,
    key: Option<IdempotencyKey>,
    TypedJsonBytes { value, bytes }: TypedJsonBytes<CategoryGroupInput>,
) -> Result<Response, Problem> {
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || idempotency::write_household(&actor);
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state
            .ledger
            .create_category_group(&actor, value, slot)
            .await
    })
    .await
}

/// Changes a category group.
#[utoipa::path(
    patch,
    path = "/api/v1/category-groups/{group_id}",
    operation_id = "updateCategoryGroup",
    tag = tags::CATEGORIES,
    description = "Changes the fields given: name, position or archived. Every category of \
                   an archived group counts as archived. Needs `write`.",
    security(("bearer" = [])),
    params(CategoryGroupPath),
    request_body = CategoryGroupPatch,
    responses(
        (status = 200, description = "The group as it now is.", body = CategoryGroupView),
        CommonProblems,
    ),
)]
pub(super) async fn update_group(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<CategoryGroupPath>,
    TypedJson(changes): TypedJson<CategoryGroupPatch>,
) -> Result<Json<CategoryGroupView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_category_group(&actor, path.group_id, changes)
            .await?,
    ))
}
