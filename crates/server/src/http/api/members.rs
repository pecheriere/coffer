//! `GET /api/v1/members`.

use axum::Json;
use axum::extract::State;
use coffer_domain::policy::{Action, authorize};
use serde_json::{Value, json};

use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::Problem;

/// Every household member, disabled ones included. Never carries passkeys
/// or tokens.
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<Value>, Problem> {
    authorize(&actor, Action::ReadHousehold).map_err(crate::auth::AuthError::from)?;
    let members = state
        .store
        .list_members()
        .await
        .map_err(crate::auth::AuthError::from)?;
    let items: Vec<Value> = members
        .iter()
        .map(|member| {
            json!({
                "id": member.id,
                "username": member.username.as_str(),
                "display_name": member.display_name.as_str(),
                "role": member.role.as_str(),
                "disabled_at": member.disabled_at.map(|at| at.to_string()),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}
