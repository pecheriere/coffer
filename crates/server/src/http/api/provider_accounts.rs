//! `/api/v1/provider-accounts`: the accounts connections report, and their
//! binding to coffer accounts.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::ids::{AccountId, ProviderAccountId};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::sync::connections::{LinkedAccountView, NewLinkedAccountInput, ProviderAccountView};

/// The provider accounts the token may see.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderAccountList {
    pub items: Vec<ProviderAccountView>,
}

/// Which provider accounts to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccountListQuery {
    /// Only those not bound to an account (default `false`).
    pub unbound: Option<bool>,
}

/// One provider account.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccountPath {
    /// The provider account's id.
    pub provider_account_id: ProviderAccountId,
}

/// The account to bind a provider account to.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BindRequest {
    /// An open account in the provider account's currency.
    pub account: AccountId,
}

/// An account to create for a provider account.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewLinkedAccountRequest {
    /// 1 to 100 characters, without control characters; by default the
    /// provider account's name.
    pub name: Option<String>,
    /// By default the kind the provider suggests; required when it suggests
    /// none.
    pub kind: Option<AccountKind>,
    /// A chosen liquidity class; by default the kind's.
    pub liquidity: Option<LiquidityClass>,
    /// Count the account in net worth (default `true`).
    pub include_in_net_worth: Option<bool>,
    /// Count the account in the forecast (default `true`).
    pub include_in_forecast: Option<bool>,
}

/// The provider accounts the token may see.
#[utoipa::path(
    get,
    path = "/api/v1/provider-accounts",
    operation_id = "listProviderAccounts",
    tag = tags::CONNECTIONS,
    description = "The accounts every connection reports, oldest first; only those not yet \
                   bound to an account with `unbound=true`. Needs `read`.",
    security(("bearer" = [])),
    params(ProviderAccountListQuery),
    responses(
        (status = 200, description = "The provider accounts.", body = ProviderAccountList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<ProviderAccountListQuery>,
) -> Result<Json<ProviderAccountList>, Problem> {
    let items = state
        .sync
        .list_provider_accounts(&actor, query.unbound.unwrap_or(false))
        .await?;
    Ok(Json(ProviderAccountList { items }))
}

/// Binds a provider account to an account.
#[utoipa::path(
    post,
    path = "/api/v1/provider-accounts/{provider_account_id}/bind",
    operation_id = "bindProviderAccount",
    tag = tags::CONNECTIONS,
    description = "Binds an unbound provider account to an open account in the same currency. \
                   The records synced while it was unbound become the account's transactions, \
                   and later syncs bring the rest. Needs `write` on the account.",
    security(("bearer" = [])),
    params(ProviderAccountPath),
    request_body = BindRequest,
    responses(
        (status = 200, description = "The provider account, now bound.",
         body = ProviderAccountView),
        (status = 409, description = "`currency_mismatch` when the currencies differ; \
                                      `conflict` when the provider account is already bound, \
                                      its connection is removed or the account is closed.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn bind(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ProviderAccountPath>,
    TypedJson(body): TypedJson<BindRequest>,
) -> Result<Json<ProviderAccountView>, Problem> {
    Ok(Json(
        state
            .sync
            .bind(&actor, path.provider_account_id, body.account)
            .await?,
    ))
}

/// Creates an account for a provider account and binds it.
#[utoipa::path(
    post,
    path = "/api/v1/provider-accounts/{provider_account_id}/create-account",
    operation_id = "createAccountForProviderAccount",
    tag = tags::CONNECTIONS,
    description = "Creates an account in the provider account's currency, exactly as \
                   `createAccount` does, and binds the provider account to it, in one step. \
                   The records synced while it was unbound become the account's transactions. \
                   Needs `write`.",
    security(("bearer" = [])),
    params(ProviderAccountPath),
    request_body = NewLinkedAccountRequest,
    responses(
        (status = 201, description = "The account and the provider account bound to it.",
         body = LinkedAccountView),
        (status = 409, description = "`currency_mismatch` when the provider's currency is not \
                                      a supported ISO 4217 code; `conflict` when the provider \
                                      account is already bound or its connection is removed.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn create_account(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ProviderAccountPath>,
    TypedJson(body): TypedJson<NewLinkedAccountRequest>,
) -> Result<(StatusCode, Json<LinkedAccountView>), Problem> {
    let input = NewLinkedAccountInput {
        name: body.name,
        kind: body.kind,
        liquidity: body.liquidity,
        include_in_net_worth: body.include_in_net_worth.unwrap_or(true),
        include_in_forecast: body.include_in_forecast.unwrap_or(true),
    };
    let linked = state
        .sync
        .create_account_and_bind(&actor, path.provider_account_id, input)
        .await?;
    Ok((StatusCode::CREATED, Json(linked)))
}
