//! The signed-in member's own passkeys and agent tokens.

use std::str::FromStr as _;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use coffer_domain::identity::{Actor, Scope, TokenName, role_ceiling};
use coffer_domain::ids::{AgentTokenId, PasskeyId};
use coffer_domain::policy::{Action, authorize};
use jiff::SignedDuration;
use maud::{Markup, html};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use super::layout::{Viewer, csrf_field, once, page};
use super::{ApiJson, PageError, PageForm, PageSession};
use crate::auth::{AuthError, CeremonyId};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::secrets::SecretToken;

/// Longest token lifetime the form accepts.
const MAX_EXPIRY_DAYS: u32 = 3650;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/account", get(account))
        .route("/account/passkeys/{id}/remove", post(remove_passkey))
        .route("/account/tokens", post(create_token))
        .route("/account/tokens/{id}/revoke", post(revoke_token))
        .route("/auth/passkeys/begin", post(begin_passkey))
        .route("/auth/passkeys/finish", post(finish_passkey))
}

/// The navigation bar's data for the signed-in member.
pub async fn viewer(state: &AppState, session: &PageSession) -> Result<Viewer, PageError> {
    let actor = session.actor();
    let member = state
        .store
        .get_member(actor.member)
        .await
        .map_err(AuthError::from)?;
    Ok(Viewer {
        display_name: member.display_name.to_string(),
        is_admin: authorize(&actor, Action::ManageMembers).is_ok(),
        csrf: session.csrf(state),
    })
}

async fn account(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    Ok(render(&state, &session, None).await?.into_response())
}

/// A token just created, whose secret is shown on the response that
/// follows and never again.
struct Created {
    name: String,
    secret: SecretToken,
}

async fn render(
    state: &AppState,
    session: &PageSession,
    created: Option<Created>,
) -> Result<Markup, PageError> {
    let actor = session.actor();
    authorize(&actor, Action::ManageOwnCredentials).map_err(AuthError::from)?;
    let viewer = viewer(state, session).await?;
    let passkeys = state
        .store
        .list_passkeys(actor.member)
        .await
        .map_err(AuthError::from)?;
    let tokens = state.agent_tokens.list_own(&actor).await?;
    let now = state.clock.now();
    let csrf = &viewer.csrf;
    let content = html! {
        h1 { "Account" }
        @if let Some(created) = &created {
            (once(
                &format!("Token \"{}\" created.", created.name),
                "new-token",
                created.secret.expose_secret(),
            ))
        }
        p #result role="status" {}

        h2 { "Passkeys" }
        table {
            tr { th { "Name" } th { "Added" } th { "Last used" } th {} }
            @for passkey in &passkeys {
                tr {
                    td { (passkey.label) }
                    td { (passkey.created_at) }
                    td { (passkey.last_used_at.map_or_else(|| "never".to_owned(), |at| at.to_string())) }
                    td {
                        form method="post" action={ "/account/passkeys/" (passkey.id) "/remove" } {
                            (csrf_field(csrf))
                            button type="submit" { "Remove" }
                        }
                    }
                }
            }
        }
        label for="passkey-label" { "Name for the new passkey" }
        input #passkey-label type="text" maxlength="64" value="passkey";
        button #add-passkey type="button" data-csrf=(csrf) { "Add a passkey" }

        h2 { "Agent tokens" }
        table {
            tr { th { "Name" } th { "Scope" } th { "Token" } th { "Expires" } th { "Status" } th {} }
            @for token in &tokens {
                @let active = token.revoked_at.is_none()
                    && token.expires_at.is_none_or(|at| at > now);
                tr {
                    td { (token.name) }
                    td { (token.scope.as_str()) }
                    td { code { (token.prefix) "…" } }
                    td { (token.expires_at.map_or_else(|| "never".to_owned(), |at| at.to_string())) }
                    td {
                        @if token.revoked_at.is_some() { "revoked" }
                        @else if !active { "expired" }
                        @else { "active" }
                    }
                    td {
                        @if active {
                            form method="post" action={ "/account/tokens/" (token.id) "/revoke" } {
                                (csrf_field(csrf))
                                button type="submit" { "Revoke" }
                            }
                        }
                    }
                }
            }
        }
        h3 { "Create a token" }
        form method="post" action="/account/tokens" {
            (csrf_field(csrf))
            label for="token-name" { "Name" }
            input #token-name type="text" name="name" maxlength="64" required;
            label for="token-scope" { "Scope" }
            select #token-scope name="scope" {
                @for scope in scopes_up_to(role_ceiling(actor.role)) {
                    option value=(scope.as_str()) { (scope.as_str()) }
                }
            }
            label for="token-expiry" { "Expires after (days, optional)" }
            input #token-expiry type="number" name="expires_days" min="1" max=(MAX_EXPIRY_DAYS);
            div { button type="submit" { "Create token" } }
        }
    };
    Ok(page("Account", Some(&viewer), &content))
}

fn scopes_up_to(ceiling: Scope) -> impl Iterator<Item = Scope> {
    [Scope::Read, Scope::Write, Scope::Admin]
        .into_iter()
        .filter(move |scope| *scope <= ceiling)
}

#[derive(Deserialize)]
struct CsrfOnly {
    #[serde(default)]
    csrf: String,
}

async fn remove_passkey(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = PasskeyId::from_str(&id).map_err(|_| PageError::not_found())?;
    state.identity.remove_passkey(&actor, id).await?;
    Ok(Redirect::to("/account"))
}

#[derive(Deserialize)]
struct NewTokenForm {
    #[serde(default)]
    csrf: String,
    name: String,
    scope: String,
    #[serde(default)]
    expires_days: String,
}

async fn create_token(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<NewTokenForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let name = TokenName::parse(&form.name).map_err(AuthError::Invalid)?;
    let scope = Scope::from_str(&form.scope).map_err(AuthError::Invalid)?;
    let expires_at = expiry(&state, &form.expires_days)?;
    let (record, secret) = state
        .agent_tokens
        .create(&actor, name, scope, expires_at)
        .await?;
    let created = Created {
        name: record.name.to_string(),
        secret,
    };
    Ok(render(&state, &session, Some(created))
        .await?
        .into_response())
}

fn expiry(state: &AppState, days: &str) -> Result<Option<jiff::Timestamp>, PageError> {
    let days = days.trim();
    if days.is_empty() {
        return Ok(None);
    }
    let days = days
        .parse::<u32>()
        .ok()
        .filter(|days| (1..=MAX_EXPIRY_DAYS).contains(days))
        .ok_or_else(|| PageError::invalid("expiry must be 1 to 3650 days"))?;
    state
        .clock
        .now()
        .checked_add(SignedDuration::from_hours(i64::from(days) * 24))
        .map(Some)
        .map_err(|_| PageError::invalid("expiry must be 1 to 3650 days"))
}

async fn revoke_token(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = AgentTokenId::from_str(&id).map_err(|_| PageError::not_found())?;
    state.agent_tokens.revoke(&actor, id).await?;
    Ok(Redirect::to("/account"))
}

/// The session and anti-forgery header of a ceremony request, as a
/// JSON-style rejection.
fn json_session(
    session: Result<PageSession, PageError>,
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Actor, Problem> {
    let session = session.map_err(|PageError(problem)| problem)?;
    let presented = headers
        .get("X-CSRF-Token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    Ok(session.verify(state, presented)?)
}

async fn begin_passkey(
    State(state): State<AppState>,
    session: Result<PageSession, PageError>,
    headers: HeaderMap,
) -> Result<Json<Value>, Problem> {
    let actor = json_session(session, &state, &headers)?;
    let (ceremony, options) = state.identity.begin_add_passkey(&actor).await?;
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

async fn finish_passkey(
    State(state): State<AppState>,
    session: Result<PageSession, PageError>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<FinishBody>,
) -> Result<Json<Value>, Problem> {
    let actor = json_session(session, &state, &headers)?;
    let ceremony = CeremonyId::from_str(&body.ceremony)?;
    let label = TokenName::parse(&body.label).map_err(AuthError::Invalid)?;
    state
        .identity
        .finish_add_passkey(&actor, ceremony, label, body.credential)
        .await?;
    Ok(Json(json!({ "redirect": "/account" })))
}
