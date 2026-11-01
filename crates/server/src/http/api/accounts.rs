//! `/api/v1/accounts`: accounts and their daily balances.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, Uri};
use axum::response::Response;
use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountLevel;
use coffer_store::PageRequest;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::accounts::{
    AccountPatch, AccountView, BalancePage, BalanceView, NewAccountInput, OpeningBalance,
};

/// Every account the token may see.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountList {
    pub items: Vec<AccountView>,
}

/// Whether to list closed accounts.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AccountListQuery {
    /// Also list closed accounts (default `false`).
    pub include_closed: Option<bool>,
}

/// One account.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct AccountPath {
    /// The account's id.
    pub account_id: AccountId,
}

/// One day of one account.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct BalancePath {
    /// The account's id.
    pub account_id: AccountId,
    /// The calendar day (ISO 8601).
    pub on: Date,
}

/// A date range and paging for an account's balances.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BalanceQuery {
    /// Only days on or after this one (ISO 8601).
    pub from: Option<Date>,
    /// Only days before this one (ISO 8601).
    pub to: Option<Date>,
    /// The `next` cursor of the previous page.
    pub after: Option<String>,
    /// The most balances to return, clamped to 1 to 200.
    #[param(minimum = 0)]
    pub limit: Option<u32>,
}

/// A manual account to create.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewAccountRequest {
    /// 1 to 100 characters, without control characters.
    pub name: String,
    pub kind: AccountKind,
    /// Fixed for the account's life.
    pub currency: Currency,
    /// A chosen liquidity class; by default the kind's (a credit card is a
    /// `short_term_liability`).
    pub liquidity: Option<LiquidityClass>,
    /// Count the account in net worth (default `true`).
    pub include_in_net_worth: Option<bool>,
    /// Count the account in the forecast (default `true`).
    pub include_in_forecast: Option<bool>,
    /// A manual balance snapshot to record with the account.
    pub opening_balance: Option<OpeningBalance>,
}

/// A manual balance for one day.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BalanceRequest {
    /// The balance in the account currency's minor units; negative is owed.
    pub minor_units: i64,
}

/// Every account the token may see.
#[utoipa::path(
    get,
    path = "/api/v1/accounts",
    operation_id = "listAccounts",
    tag = tags::ACCOUNTS,
    description = "Every account, oldest first, each with its latest balance. Closed \
                   accounts are listed only with `include_closed=true`. Needs `read`.",
    security(("bearer" = [])),
    params(AccountListQuery),
    responses(
        (status = 200, description = "The accounts.", body = AccountList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<AccountListQuery>,
) -> Result<Json<AccountList>, Problem> {
    let items = state
        .ledger
        .list_accounts(&actor, query.include_closed.unwrap_or(false))
        .await?;
    Ok(Json(AccountList { items }))
}

/// Creates a manual account.
#[utoipa::path(
    post,
    path = "/api/v1/accounts",
    operation_id = "createAccount",
    tag = tags::ACCOUNTS,
    description = "Creates a manual account, with an opening balance if given. Needs \
                   `write`; a token with account grants also needs `all` at `write`. With an `Idempotency-Key`, a retry of the same request within 24 \
                   hours returns the first response instead of creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        token (or session) for 24 hours."),
    ),
    request_body = NewAccountRequest,
    responses(
        (status = 201, description = "The account, created now or by an earlier request \
                                      with the same `Idempotency-Key`.", body = AccountView),
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
    TypedJsonBytes { value, bytes }: TypedJsonBytes<NewAccountRequest>,
) -> Result<Response, Problem> {
    let input = NewAccountInput {
        name: value.name,
        kind: value.kind,
        currency: value.currency,
        liquidity: value.liquidity,
        include_in_net_worth: value.include_in_net_worth.unwrap_or(true),
        include_in_forecast: value.include_in_forecast.unwrap_or(true),
        opening_balance: value.opening_balance,
    };
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || {
        idempotency::write_household(&actor)?;
        Ok(state
            .ledger
            .authorize_every_account_access(&actor, AccountLevel::Write)
            .await?)
    };
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state.ledger.create_account(&actor, input, slot).await
    })
    .await
}

/// One account.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/{account_id}",
    operation_id = "getAccount",
    tag = tags::ACCOUNTS,
    description = "One account with its latest balance. Needs `read`.",
    security(("bearer" = [])),
    params(AccountPath),
    responses(
        (status = 200, description = "The account.", body = AccountView),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<AccountPath>,
) -> Result<Json<AccountView>, Problem> {
    Ok(Json(
        state.ledger.get_account(&actor, path.account_id).await?,
    ))
}

/// Changes an account.
#[utoipa::path(
    patch,
    path = "/api/v1/accounts/{account_id}",
    operation_id = "updateAccount",
    tag = tags::ACCOUNTS,
    description = "Changes the fields given. The currency cannot change. A new `kind` \
                   without `liquidity` re-defaults the liquidity class unless one was chosen. \
                   Needs `write` on the account.",
    security(("bearer" = [])),
    params(AccountPath),
    request_body = AccountPatch,
    responses(
        (status = 200, description = "The account as it now is.", body = AccountView),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<AccountPath>,
    TypedJson(changes): TypedJson<AccountPatch>,
) -> Result<Json<AccountView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_account(&actor, path.account_id, changes)
            .await?,
    ))
}

/// An account's balances.
#[utoipa::path(
    get,
    path = "/api/v1/accounts/{account_id}/balances",
    operation_id = "listBalances",
    tag = tags::ACCOUNTS,
    description = "An account's daily balances, newest day first: one per day, the last \
                   write winning. Needs `read`. Pages with the opaque `next` cursor.",
    security(("bearer" = [])),
    params(AccountPath, BalanceQuery),
    responses(
        (status = 200, description = "One page of balances.", body = BalancePage),
        CommonProblems,
    ),
)]
pub(super) async fn list_balances(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<AccountPath>,
    TypedQuery(query): TypedQuery<BalanceQuery>,
) -> Result<Json<BalancePage>, Problem> {
    let mut page = PageRequest {
        after: query.after,
        ..PageRequest::default()
    };
    if let Some(limit) = query.limit {
        page.limit = limit;
    }
    Ok(Json(
        state
            .ledger
            .list_balances(&actor, path.account_id, query.from, query.to, &page)
            .await?,
    ))
}

/// Records a manual balance.
#[utoipa::path(
    put,
    path = "/api/v1/accounts/{account_id}/balances/{on}",
    operation_id = "setBalance",
    tag = tags::ACCOUNTS,
    description = "Records the account's balance at the end of `on`, replacing that day's \
                   balance whatever its source. A linked account's next sync replaces it in \
                   turn. Needs `write` on the account.",
    security(("bearer" = [])),
    params(BalancePath),
    request_body = BalanceRequest,
    responses(
        (status = 200, description = "The recorded balance.", body = BalanceView),
        CommonProblems,
    ),
)]
pub(super) async fn set_balance(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<BalancePath>,
    TypedJson(body): TypedJson<BalanceRequest>,
) -> Result<Json<BalanceView>, Problem> {
    Ok(Json(
        state
            .ledger
            .set_manual_balance(&actor, path.account_id, path.on, body.minor_units)
            .await?,
    ))
}
