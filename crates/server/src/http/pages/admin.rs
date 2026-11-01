//! Household administration: members, their agent tokens and invites.
//! Authorization is enforced by the services, and by the handlers for
//! admin-only routes over actions members may also take on their own
//! behalf; a denial renders as a 403 page.

use std::str::FromStr as _;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use coffer_domain::identity::{DisplayName, Role};
use coffer_domain::ids::{AgentTokenId, InviteId, MemberId};
use coffer_domain::policy::{Action, authorize};
use coffer_store::InviteKind;
use maud::{Markup, html};
use serde::Deserialize;
use url::Url;

use super::account::{token_item, viewer};
use super::layout::{Frame, Nav, csrf_field, empty_state, once, page};
use super::{PageError, PageForm, PageSession};
use crate::auth::AuthError;
use crate::http::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/members", get(members))
        .route("/admin/members/{id}/role", post(set_role))
        .route("/admin/members/{id}/disable", post(disable))
        .route("/admin/members/{id}/reenroll", post(reenroll))
        .route("/admin/tokens/{id}/revoke", post(revoke_token))
        .route("/admin/invites", get(invites).post(create_invite))
        .route("/admin/invites/{id}/revoke", post(revoke_invite))
}

async fn members(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    Ok(render_members(&state, &session, None)
        .await?
        .into_response())
}

async fn render_members(
    state: &AppState,
    session: &PageSession,
    link: Option<&Url>,
) -> Result<Markup, PageError> {
    let actor = session.actor();
    authorize(&actor, Action::ManageMembers).map_err(AuthError::from)?;
    let viewer = viewer(state, session);
    let look = session.look(state).await;
    let members = state.store.list_members().await.map_err(AuthError::from)?;
    let tokens = state.agent_tokens.list_all(&actor).await?;
    let username = |id: MemberId| {
        members
            .iter()
            .find(|member| member.id == id)
            .map_or("unknown", |member| member.username.as_str())
    };
    let now = state.clock.now();
    let csrf = &viewer.csrf;
    let content = html! {
        h1 { "Members" }
        @if let Some(link) = link {
            (once("Re-enroll link created.", "new-link", link.as_str()))
        }
        section.card {
            h2 { "People" }
            ul.items {
                @for member in &members {
                    li.item {
                        div.item-main {
                            span.item-title { (member.username.as_str()) }
                            span.item-meta { (member.display_name) }
                        }
                        span.chip { (member.role.as_str()) }
                        @if member.disabled_at.is_some() {
                            span.chip.chip-quiet { "disabled" }
                        } @else {
                            span.chip { "active" }
                            div.item-actions {
                                form method="post" action={ "/admin/members/" (member.id) "/role" } {
                                    (csrf_field(csrf))
                                    select.input name="role"
                                        aria-label={ "Role for " (member.username.as_str()) } {
                                        @for role in [Role::Member, Role::Admin] {
                                            option value=(role.as_str()) selected[role == member.role] {
                                                (role.as_str())
                                            }
                                        }
                                    }
                                    button.btn type="submit" { "Set role" }
                                }
                                form method="post" action={ "/admin/members/" (member.id) "/reenroll" } {
                                    (csrf_field(csrf))
                                    button.btn type="submit" { "Re-enroll link" }
                                }
                                form method="post" action={ "/admin/members/" (member.id) "/disable" } {
                                    (csrf_field(csrf))
                                    button.btn type="submit" { "Disable" }
                                }
                            }
                        }
                    }
                }
            }
        }
        section.card {
            h2 { "Agent tokens" }
            @if tokens.is_empty() {
                (empty_state(look, "No agent tokens in this household."))
            } @else {
                ul.items {
                    @for token in &tokens {
                        (token_item(token, now, csrf, &format!("/admin/tokens/{}/revoke", token.id), &html! {
                            (username(token.member)) " · " code { (token.prefix) "…" }
                            " · Created " (token.created_at) " · Last used "
                            (token.last_used_at.map_or_else(|| "never".to_owned(), |at| at.to_string()))
                        }, &html! {}))
                    }
                }
            }
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Members),
        narrow: false,
    };
    Ok(page(&frame, "Members", &content))
}

#[derive(Deserialize)]
struct RoleForm {
    #[serde(default)]
    csrf: String,
    role: String,
}

#[derive(Deserialize)]
struct CsrfOnly {
    #[serde(default)]
    csrf: String,
}

fn parse_id<T: std::str::FromStr>(id: &str) -> Result<T, PageError> {
    id.parse().map_err(|_| PageError::not_found())
}

async fn set_role(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<RoleForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let member: MemberId = parse_id(&id)?;
    let role = Role::from_str(&form.role).map_err(AuthError::Invalid)?;
    state.identity.set_role(&actor, member, role).await?;
    Ok(Redirect::to("/admin/members"))
}

async fn disable(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let member: MemberId = parse_id(&id)?;
    state.identity.disable_member(&actor, member).await?;
    Ok(Redirect::to("/admin/members"))
}

/// Revokes any member's agent token. Members revoke their own from the
/// account page; this route is for admins only.
async fn revoke_token(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    authorize(&actor, Action::ManageMembers).map_err(AuthError::from)?;
    let token: AgentTokenId = parse_id(&id)?;
    state.agent_tokens.revoke(&actor, token).await?;
    Ok(Redirect::to("/admin/members"))
}

async fn reenroll(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let member: MemberId = parse_id(&id)?;
    let link = state.identity.create_reenroll(&actor, member).await?;
    Ok(render_members(&state, &session, Some(&link))
        .await?
        .into_response())
}

async fn invites(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    Ok(render_invites(&state, &session, None)
        .await?
        .into_response())
}

async fn render_invites(
    state: &AppState,
    session: &PageSession,
    link: Option<&Url>,
) -> Result<Markup, PageError> {
    let actor = session.actor();
    let pending = state.identity.list_pending_invites(&actor).await?;
    let viewer = viewer(state, session);
    let look = session.look(state).await;
    let csrf = &viewer.csrf;
    let content = html! {
        h1 { "Invites" }
        @if let Some(link) = link {
            (once("Invite link created.", "new-link", link.as_str()))
        }
        section.card {
            h2 { "Invite someone" }
            form method="post" action="/admin/invites" {
                (csrf_field(csrf))
                label.field {
                    "Role"
                    select #invite-role .input name="role" {
                        option value="member" { "member" }
                        option value="admin" { "admin" }
                    }
                }
                label.field {
                    "Display name (optional suggestion)"
                    input #invite-name .input type="text" name="display_name" maxlength="64";
                }
                button.btn.btn-primary type="submit" { "Create invite link" }
            }
        }
        section.card {
            h2 { "Pending invites" }
            @if pending.is_empty() {
                (empty_state(look, "No pending invites."))
            } @else {
                ul.items {
                    @for invite in &pending {
                        li.item {
                            div.item-main {
                                span.item-title {
                                    @match &invite.kind {
                                        InviteKind::Bootstrap => { "first admin" }
                                        InviteKind::NewMember { role, display_name } => {
                                            (role.as_str())
                                            @if let Some(name) = display_name { " (" (name) ")" }
                                        }
                                        InviteKind::Reenroll { member } => { "re-enroll " (member) }
                                    }
                                }
                                span.item-meta {
                                    "Created " (invite.created_at) " · Expires " (invite.expires_at)
                                }
                            }
                            div.item-actions {
                                form method="post" action={ "/admin/invites/" (invite.id) "/revoke" } {
                                    (csrf_field(csrf))
                                    button.btn type="submit" { "Revoke" }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Invites),
        narrow: false,
    };
    Ok(page(&frame, "Invites", &content))
}

#[derive(Deserialize)]
struct InviteForm {
    #[serde(default)]
    csrf: String,
    role: String,
    #[serde(default)]
    display_name: String,
}

async fn create_invite(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<InviteForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let role = Role::from_str(&form.role).map_err(AuthError::Invalid)?;
    let name = form.display_name.trim();
    let display_name = (!name.is_empty())
        .then(|| DisplayName::parse(name))
        .transpose()
        .map_err(AuthError::Invalid)?;
    let link = state
        .identity
        .create_invite(&actor, role, display_name)
        .await?;
    Ok(render_invites(&state, &session, Some(&link))
        .await?
        .into_response())
}

async fn revoke_invite(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfOnly>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let invite: InviteId = parse_id(&id)?;
    state.identity.revoke_invite(&actor, invite).await?;
    Ok(Redirect::to("/admin/invites"))
}
