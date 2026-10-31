//! `GET /api/v1/me`.

use axum::Json;
use axum::extract::State;
use coffer_domain::identity::Scope;
use coffer_domain::ids::AgentTokenId;
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

/// Who the bearer token belongs to.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    operation_id = "getMe",
    tag = tags::IDENTITY,
    description = "The member who created the bearer token, and the token itself.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The token's member and the token.", body = MeResponse),
        CommonProblems,
    ),
)]
pub(super) async fn me(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<MeResponse>, Problem> {
    let (member, token) = state.agent_tokens.describe(&actor).await?;
    Ok(Json(MeResponse {
        member: MemberView::from(&member),
        agent: AgentView {
            id: token.id,
            name: token.name.as_str().to_owned(),
            scope: token.scope,
        },
    }))
}
