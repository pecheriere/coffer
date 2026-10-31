//! `GET /api/v1/me`.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::Problem;

/// Who the bearer token belongs to.
pub(super) async fn me(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<Value>, Problem> {
    let (member, token) = state.agent_tokens.describe(&actor).await?;
    Ok(Json(json!({
        "member": {
            "id": member.id,
            "username": member.username.as_str(),
            "display_name": member.display_name.as_str(),
            "role": member.role.as_str(),
        },
        "agent": {
            "id": token.id,
            "name": token.name.as_str(),
            "scope": token.scope.as_str(),
        },
    })))
}
