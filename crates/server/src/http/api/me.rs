//! `GET /api/v1/me`.

use axum::Json;
use axum::extract::State;
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::policy::AccountLevel;
use serde::Serialize;
use utoipa::ToSchema;

use super::members::MemberView;
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};

/// The member and agent token behind the bearer token.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub member: MemberView,
    pub agent: AgentView,
    /// Which accounts the token may use.
    pub access: AccessView,
}

/// An agent token, without its secret.
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentView {
    pub id: AgentTokenId,
    /// The name its member gave it.
    pub name: String,
    /// The rights it carries, never more than its member's role.
    pub scope: Scope,
}

/// The accounts a token may use. A token without grants shows `all` equal
/// to `ceiling` and no `accounts`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccessView {
    /// The most the token's scope allows on any account.
    pub ceiling: AccountLevel,
    /// The level on every account, including ones created later; `null`
    /// without an `all` grant.
    #[schema(required)]
    pub all: Option<AccountLevel>,
    /// The accounts a grant names, and the level really held on each. An
    /// account neither here nor covered by `all` is hidden from the token.
    pub accounts: Vec<AccountGrantView>,
}

/// The level a token holds on one account.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountGrantView {
    pub account: AccountId,
    pub level: AccountLevel,
}

/// Who the bearer token belongs to.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    operation_id = "getMe",
    tag = tags::IDENTITY,
    description = "The member who created the bearer token, the token itself, and the accounts \
                   it may use.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The token's member, the token and its access.",
         body = MeResponse),
        CommonProblems,
    ),
)]
pub(super) async fn me(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<MeResponse>, Problem> {
    let (member, token) = state.agent_tokens.describe(&actor).await?;
    let access = state.access.resolve(&actor).await?.summary();
    Ok(Json(MeResponse {
        member: MemberView::from(&member),
        agent: AgentView {
            id: token.id,
            name: token.name.as_str().to_owned(),
            scope: token.scope,
        },
        access: AccessView {
            ceiling: access.ceiling,
            all: access.all,
            accounts: access
                .accounts
                .into_iter()
                .map(|(account, level)| AccountGrantView { account, level })
                .collect(),
        },
    }))
}
