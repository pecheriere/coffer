//! Sign-in and sign-out.

use std::str::FromStr as _;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use coffer_domain::identity::Username;
use maud::html;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::PublicKeyCredential;

use super::layout::{Frame, auth_card, key_icon, page};
use super::{ApiJson, PageError, PageForm, PageSession, start_session};
use crate::auth::{AuthError, CeremonyId, clear_session_cookie};
use crate::http::AppState;
use crate::http::problem::Problem;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_page))
        .route("/auth/login/begin", post(begin))
        .route("/auth/login/finish", post(finish))
        .route("/logout", post(logout))
}

async fn login_page(State(state): State<AppState>) -> Response {
    let look = state.appearance.look(None).await;
    let form = html! {
        label.field {
            "Username"
            input #username .input type="text" autocomplete="username webauthn" maxlength="32"
                required;
        }
        button #login .btn .btn-primary .btn-block type="button" {
            (key_icon())
            "Sign in with a passkey"
        }
        p #result role="status" {}
        p.fine { "No passwords. Face ID, Touch ID, Windows Hello or a security key." }
    };
    let frame = Frame {
        look,
        viewer: None,
        current: None,
        narrow: true,
    };
    page(&frame, "Sign in", &auth_card(look, "Welcome home", &form)).into_response()
}

#[derive(Deserialize)]
struct BeginBody {
    username: String,
}

async fn begin(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<BeginBody>,
) -> Result<Json<Value>, Problem> {
    let username = Username::parse(body.username.trim()).map_err(AuthError::Invalid)?;
    let (ceremony, options) = state.identity.begin_login(&username).await?;
    Ok(Json(
        json!({ "ceremony": ceremony.as_str(), "options": options }),
    ))
}

#[derive(Deserialize)]
struct FinishBody {
    ceremony: String,
    credential: PublicKeyCredential,
}

async fn finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<FinishBody>,
) -> Result<Response, Problem> {
    let ceremony = CeremonyId::from_str(&body.ceremony)?;
    let member = state
        .identity
        .finish_login(ceremony, body.credential)
        .await?;
    Ok(start_session(&state, &headers, member).await?)
}

#[derive(Deserialize)]
struct LogoutForm {
    #[serde(default)]
    csrf: String,
}

async fn logout(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<LogoutForm>,
) -> Result<Response, PageError> {
    session.verify(&state, &form.csrf)?;
    state.sessions.logout(session.cookie()).await?;
    Ok((
        [(axum::http::header::SET_COOKIE, clear_session_cookie())],
        Redirect::to("/login"),
    )
        .into_response())
}
