//! The signed-in member's own passkeys, agent tokens and appearance.

use std::collections::BTreeMap;
use std::str::FromStr as _;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use coffer_domain::identity::{Actor, ColorScheme, Scope, ThemeName, TokenName, role_ceiling};
use coffer_domain::ids::{AgentTokenId, PasskeyId};
use coffer_domain::policy::{
    AccountLevel, Action, TokenGrant, authorize, grant_ceiling, validate_grants,
};
use coffer_store::{AgentTokenRecord, PasskeyRecord};
use jiff::{SignedDuration, Timestamp};
use maud::{Markup, html};
use secrecy::ExposeSecret as _;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use super::grants::{
    Back, GrantChoices, LIMIT_HELP, account_field, grants_editor, grants_summary, level_field,
};
use super::layout::{Frame, Nav, Viewer, csrf_field, empty_state, once, page};
use super::{ApiJson, PageError, PageForm, PageSession};
use crate::auth::{AuthError, CeremonyId};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::accounts::AccountView;
use crate::secrets::SecretToken;
use crate::themes::{self, THEMES, Theme};

/// Longest token lifetime the form accepts.
const MAX_EXPIRY_DAYS: u32 = 3650;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/account", get(account))
        .route("/account/appearance", post(set_appearance))
        .route("/account/passkeys/{id}/remove", post(remove_passkey))
        .route("/account/tokens", post(create_token))
        .route("/account/tokens/{id}/revoke", post(revoke_token))
        .route("/auth/passkeys/begin", post(begin_passkey))
        .route("/auth/passkeys/finish", post(finish_passkey))
}

/// The navigation bar's data for the signed-in member.
pub fn viewer(state: &AppState, session: &PageSession) -> Viewer {
    Viewer {
        display_name: session.member().display_name.to_string(),
        can_manage_members: authorize(&session.actor(), Action::ManageMembers).is_ok(),
        can_manage_household: authorize(&session.actor(), Action::ManageHousehold).is_ok(),
        csrf: session.csrf(state),
    }
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
    let viewer = viewer(state, session);
    let look = session.look(state).await;
    let passkeys = state
        .store
        .list_passkeys(actor.member)
        .await
        .map_err(AuthError::from)?;
    let mut tokens = Vec::new();
    for token in state.agent_tokens.list_own(&actor).await? {
        let grants = state
            .store
            .token_grants(token.id)
            .await
            .map_err(AuthError::from)?;
        tokens.push((token, grants));
    }
    let accounts = state
        .ledger
        .list_accounts(&actor, true)
        .await
        .map_err(|err| PageError(err.into()))?;
    let household = state.appearance.household_theme().await;
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

        (passkeys_card(&passkeys, csrf))
        (appearance_card(session, household, csrf))

        section.card {
            h2 { "Agent tokens" }
            @if tokens.is_empty() {
                (empty_state(look, "No agent tokens yet. Create one below to let an agent use coffer on your behalf."))
            } @else {
                ul.items {
                    @for (token, grants) in &tokens {
                        (token_item(token, now, csrf, &format!("/account/tokens/{}/revoke", token.id), &html! {
                            code { (token.prefix) "…" }
                            " · Expires "
                            (token.expires_at.map_or_else(|| "never".to_owned(), |at| at.to_string()))
                            " · " (grants_summary(grants, &accounts))
                        }, &grants_editor(token, grants, &accounts, now, csrf, Back::Account)))
                    }
                }
            }
            (new_token_form(role_ceiling(actor.role), &accounts, csrf))
        }
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Account),
        narrow: false,
    };
    Ok(page(&frame, "Account", &content))
}

fn new_token_form(ceiling: Scope, accounts: &[AccountView], csrf: &str) -> Markup {
    html! {
        h3 { "Create a token" }
        form method="post" action="/account/tokens" {
            (csrf_field(csrf))
            label.field {
                "Name"
                input #token-name .input type="text" name="name" maxlength="64" required;
            }
            label.field {
                "Scope"
                select #token-scope .input name="scope" {
                    @for scope in scopes_up_to(ceiling) {
                        option value=(scope.as_str()) { (scope.as_str()) }
                    }
                }
            }
            label.field {
                "Expires after (days, optional)"
                input #token-expiry .input type="number" name="expires_days" min="1"
                    max=(MAX_EXPIRY_DAYS);
            }
            p.item-meta {
                (LIMIT_HELP)
            }
            (level_field("All accounts", "all", Some(AccountLevel::Write), AccountLevel::Write))
            @for account in accounts.iter().filter(|account| !account.closed) {
                (level_field(&account.name, &account_field(account.id), None, AccountLevel::Write))
            }
            button.btn.btn-primary type="submit" { "Create token" }
        }
    }
}

fn passkeys_card(passkeys: &[PasskeyRecord], csrf: &str) -> Markup {
    html! {
        section.card {
            h2 { "Passkeys" }
            ul.items {
                @for passkey in passkeys {
                    li.item {
                        div.item-main {
                            span.item-title { (passkey.label) }
                            span.item-meta {
                                "Added " (passkey.created_at) " · Last used "
                                (passkey.last_used_at.map_or_else(|| "never".to_owned(), |at| at.to_string()))
                            }
                        }
                        div.item-actions {
                            form method="post" action={ "/account/passkeys/" (passkey.id) "/remove" } {
                                (csrf_field(csrf))
                                button.btn type="submit" { "Remove" }
                            }
                        }
                    }
                }
            }
            label.field {
                "Name for the new passkey"
                input #passkey-label .input type="text" maxlength="64" value="passkey";
            }
            button #add-passkey .btn type="button" data-csrf=(csrf) { "Add a passkey" }
        }
    }
}

/// The member's theme and colour scheme. The household default is checked
/// when the stored theme is unset or no longer registered.
fn appearance_card(session: &PageSession, household: &Theme, csrf: &str) -> Markup {
    let member = session.member();
    let own = member
        .theme
        .as_ref()
        .and_then(|name| themes::find(name.as_str()))
        .map(|theme| theme.name);
    let schemes = [
        (ColorScheme::System, "Match this device"),
        (ColorScheme::Light, "Light"),
        (ColorScheme::Dark, "Dark"),
    ];
    html! {
        section.card {
            h2 { "Appearance" }
            form method="post" action="/account/appearance" {
                (csrf_field(csrf))
                fieldset.choices {
                    legend { "Theme" }
                    label.choice {
                        input type="radio" name="theme" value="" checked[own.is_none()];
                        "Household default (" (household.label) ")"
                    }
                    @for theme in THEMES {
                        label.choice {
                            input type="radio" name="theme" value=(theme.name)
                                checked[own == Some(theme.name)];
                            (theme.label)
                        }
                    }
                }
                fieldset.choices {
                    legend { "Light or dark" }
                    @for (scheme, label) in schemes {
                        label.choice {
                            input type="radio" name="color_scheme" value=(scheme.as_str())
                                checked[member.color_scheme == scheme];
                            (label)
                        }
                    }
                }
                button.btn.btn-primary type="submit" { "Save appearance" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppearanceForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    theme: String,
    #[serde(default)]
    color_scheme: String,
}

/// Sets the member's own theme (empty follows the household) and colour
/// scheme. The policy is checked before the values are parsed, so a 403
/// always comes before a 422. A value that does not parse, or a theme that
/// is not registered, is refused whole and never echoed.
async fn set_appearance(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<AppearanceForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    authorize(&actor, Action::ManageOwnPreferences).map_err(AuthError::from)?;
    let theme = (!form.theme.is_empty())
        .then(|| ThemeName::parse(&form.theme))
        .transpose()
        .map_err(AuthError::Invalid)?;
    let scheme = ColorScheme::from_str(&form.color_scheme).map_err(AuthError::Invalid)?;
    state.appearance.set_member(&actor, theme, scheme).await?;
    Ok(Redirect::to("/account"))
}

/// One agent token in a list: its name and `meta`, chips for its scope and
/// status (quiet once it no longer works), and a revoke form posting to
/// `revoke` while it is active, then `detail` across the item's full width.
pub(super) fn token_item(
    token: &AgentTokenRecord,
    now: Timestamp,
    csrf: &str,
    revoke: &str,
    meta: &Markup,
    detail: &Markup,
) -> Markup {
    let active = token_active(token, now);
    html! {
        li.item {
            div.item-main {
                span.item-title { (token.name) }
                span.item-meta { (meta) }
            }
            span.chip { (token.scope.as_str()) }
            @if token.revoked_at.is_some() { span.chip.chip-quiet { "revoked" } }
            @else if !active { span.chip.chip-quiet { "expired" } }
            @else { span.chip { "active" } }
            @if active {
                div.item-actions {
                    form method="post" action=(revoke) {
                        (csrf_field(csrf))
                        button.btn type="submit" { "Revoke" }
                    }
                }
            }
            (detail)
        }
    }
}

pub(super) fn token_active(token: &AgentTokenRecord, now: Timestamp) -> bool {
    token.revoked_at.is_none() && token.expires_at.is_none_or(|at| at > now)
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
    /// The level selects, see [`GrantChoices`].
    #[serde(flatten)]
    levels: BTreeMap<String, String>,
}

/// The grants of a new token. All accounts is capped at the scope, so the
/// form's default fits every scope, and at the scope's ceiling it means no
/// restriction: no grants. A form without selects is the same. Every level
/// `none` is refused rather than read as no restriction.
fn new_token_grants(
    scope: Scope,
    levels: &BTreeMap<String, String>,
) -> Result<Vec<TokenGrant>, PageError> {
    if levels.is_empty() {
        return Ok(Vec::new());
    }
    let ceiling = grant_ceiling(scope);
    let mut choices = GrantChoices::parse(levels)?;
    choices.all = choices.all.map(|level| level.min(ceiling));
    let unrestricted = choices.all == Some(ceiling);
    let grants = choices.into_grants()?;
    validate_grants(scope, &grants).map_err(AuthError::InvalidGrants)?;
    Ok(if unrestricted { Vec::new() } else { grants })
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
    let grants = new_token_grants(scope, &form.levels)?;
    let (record, secret) = state
        .agent_tokens
        .create(&actor, name, scope, expires_at, grants)
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
pub(super) fn json_session(
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
