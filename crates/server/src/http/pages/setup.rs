//! First-admin setup, new-member invite and re-enroll links: one page and one
//! ceremony for all three. The link's token is in the URL path.

use std::str::FromStr as _;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use coffer_domain::identity::{DisplayName, TokenName, Username};
use maud::html;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use super::layout::page;
use super::{ApiJson, PageError, start_session};
use crate::auth::{AuthError, CeremonyId, InvitePreview};
use crate::http::AppState;
use crate::http::problem::Problem;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/setup/{token}", get(link_page))
        .route("/invite/{token}", get(link_page))
        .route("/auth/enroll/begin", post(begin))
        .route("/auth/enroll/finish", post(finish))
}

async fn link_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, PageError> {
    let preview = state.identity.describe_invite(&token).await?;
    let content = html! {
        h1 { "Create your passkey" }
        @match &preview {
            InvitePreview::NewMember { display_name } => {
                p { "Choose how you sign in to coffer." }
                label for="username" { "Username" }
                input #username name="username" type="text" autocomplete="username" maxlength="32"
                    pattern="[a-z0-9_\\-]+" required;
                label for="display-name" { "Display name" }
                input #display-name name="display_name" type="text" maxlength="64"
                    value=(display_name.as_ref().map(DisplayName::as_str).unwrap_or_default());
            }
            InvitePreview::Reenroll => {
                p { "Add a new passkey to your existing account." }
            }
        }
        label for="label" { "Name for this passkey" }
        input #label type="text" maxlength="64" value="passkey";
        div { button #enroll type="button" { "Create passkey" } }
        p #result role="status" {}
    };
    Ok(page("Create your passkey", None, &content).into_response())
}

#[derive(Deserialize)]
struct BeginBody {
    token: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
}

async fn begin(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<BeginBody>,
) -> Result<Json<Value>, Problem> {
    let username = non_empty(&body.username)
        .map(Username::parse)
        .transpose()
        .map_err(AuthError::Invalid)?;
    let display_name = non_empty(&body.display_name)
        .map(DisplayName::parse)
        .transpose()
        .map_err(AuthError::Invalid)?;
    let (ceremony, options) = state
        .identity
        .begin_enrollment(&body.token, username, display_name)
        .await?;
    Ok(Json(
        json!({ "ceremony": ceremony.as_str(), "options": options }),
    ))
}

#[derive(Deserialize)]
struct FinishBody {
    ceremony: String,
    label: String,
    credential: RegisterPublicKeyCredential,
}

async fn finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<FinishBody>,
) -> Result<Response, Problem> {
    let ceremony = CeremonyId::from_str(&body.ceremony)?;
    let label = TokenName::parse(&body.label).map_err(AuthError::Invalid)?;
    let member = state
        .identity
        .finish_enrollment(ceremony, label, body.credential)
        .await?;
    Ok(start_session(&state, &headers, member).await?)
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
