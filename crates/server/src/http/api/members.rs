//! `GET /api/v1/members`.

use axum::Json;
use axum::extract::State;
use coffer_domain::identity::Role;
use coffer_domain::ids::MemberId;
use coffer_domain::policy::{Action, authorize};
use coffer_store::Member;
use jiff::Timestamp;
use serde::Serialize;
use utoipa::ToSchema;

use super::openapi::tags;
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem};

/// A household member. Never carries passkeys or tokens.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemberView {
    pub id: MemberId,
    /// The sign-in name: lowercase, unique in the household.
    pub username: String,
    /// The name shown to other members.
    pub display_name: String,
    pub role: Role,
    /// When the member was disabled; `null` while active.
    #[schema(required)]
    pub disabled_at: Option<Timestamp>,
}

impl From<&Member> for MemberView {
    fn from(member: &Member) -> Self {
        Self {
            id: member.id,
            username: member.username.as_str().to_owned(),
            display_name: member.display_name.as_str().to_owned(),
            role: member.role,
            disabled_at: member.disabled_at,
        }
    }
}

/// Every household member.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemberList {
    pub items: Vec<MemberView>,
}

/// Every household member, disabled ones included.
#[utoipa::path(
    get,
    path = "/api/v1/members",
    operation_id = "listMembers",
    tag = tags::MEMBERS,
    description = "Every household member, disabled ones included. Needs `read`.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Every member.", body = MemberList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<MemberList>, Problem> {
    authorize(&actor, Action::ReadHousehold)?;
    let members = state.store.list_members().await.map_err(AuthError::from)?;
    Ok(Json(MemberList {
        items: members.iter().map(MemberView::from).collect(),
    }))
}
