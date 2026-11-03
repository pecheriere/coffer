//! Connections to data providers: each with its status, last sync and
//! errors, and the provider accounts it reports by name and binding. No
//! balance or transaction is shown (spec §6.4: the forecast page is the only
//! one with financial data). Forms add a `SimpleFin` connection, reconnect one that needs it,
//! request a sync, remove a connection, forget one whose sealed token
//! cannot be opened (see [`forget`]), let a held sync cursor move past the
//! records that keep failing, bind a provider account to an
//! account (see [`bind`]; each card is drawn by [`card`]) and say what a bound account is and whose money it is
//! (see [`belonging`]); each needs the session's
//! anti-forgery token, and a setup token is never shown back. When Plaid is
//! configured it links to the Plaid Link pages.

mod belonging;
mod bind;
mod card;
mod dismiss;
mod forget;

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use coffer_domain::policy::{Action, authorize};
use coffer_domain::sync::SyncTrigger;
use jiff::SignedDuration;
use maud::html;
use secrecy::SecretString;
use serde::Deserialize;

use self::card::{Permissions, Shared, connection_card};
use super::account::viewer;
use super::layout::{Frame, Nav, csrf_field, empty_state, page};
use super::{PageError, PageForm, PageSession};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::problem::{Problem, set_retry_after};
use crate::ledger::accounts::AccountView;
use crate::sync::provider::{ConnectInput, ProviderError, ProviderKind};
use crate::sync::{SyncError, retry_seconds};

const LABEL_INVALID: &str = "The label must be 1 to 100 characters without control characters.";
const RELEASE_UNREADABLE: &str = "the release form could not be read";
const TOKEN_MISSING: &str = "Paste the setup token from the SimpleFin Bridge.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/connections", get(connections))
        .route("/connections/simplefin", post(add_simplefin))
        .route(
            "/connections/{id}/simplefin/reconnect",
            post(reconnect_simplefin),
        )
        .route("/connections/{id}/sync", post(sync))
        .route("/connections/{id}/remove", post(remove))
        .route("/connections/{id}/forget", post(forget::forget))
        .route("/connections/{id}/release-cursor", post(release_cursor))
        .route(
            "/connections/accounts/{id}/belonging",
            post(belonging::update),
        )
        .route(
            "/connections/provider-accounts/{id}/create-account",
            post(bind::create_account),
        )
        .route(
            "/connections/provider-accounts/{id}/bind",
            post(bind::link_account),
        )
        .route(
            "/connections/provider-accounts/{id}/dismiss",
            post(dismiss::dismiss),
        )
        .route(
            "/connections/provider-accounts/{id}/restore",
            post(dismiss::restore),
        )
}

/// A message about a form that did not go through, shown above the list
/// with the response's status.
struct Notice {
    status: StatusCode,
    message: String,
    /// Seconds until a retry may succeed, sent as `Retry-After`.
    retry_after: Option<u32>,
    /// The provider account whose bind form was refused, when one was.
    refused_row: Option<ProviderAccountId>,
}

impl Notice {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            refused_row: None,
        }
    }

    /// This notice, about a bind form of the provider account `row`.
    fn on_row(self, row: ProviderAccountId) -> Self {
        Self {
            refused_row: Some(row),
            ..self
        }
    }
}

async fn connections(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    render(&state, &session, None).await
}

async fn render(
    state: &AppState,
    session: &PageSession,
    notice: Option<Notice>,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let viewer = viewer(state, session).await;
    let look = session.look(state).await;
    let connections = state
        .sync
        .list_connections(&actor)
        .await
        .map_err(page_error)?;
    let accounts = state
        .ledger
        .list_accounts(&actor, true)
        .await
        .map_err(|err| PageError(err.into()))?;
    let binding = bind::Binding::load(state, &actor, &accounts).await?;
    let account_views: HashMap<AccountId, &AccountView> = accounts
        .iter()
        .map(|account| (account.id, account))
        .collect();
    let can_manage = authorize(&actor, Action::ManageConnections).is_ok();
    let csrf = &viewer.csrf;
    let plaid_offered = state.config.plaid.is_some();
    let forgettable = state.sync.forgettable(&actor).await.map_err(page_error)?;
    let timezone = state
        .ledger
        .timezone()
        .await
        .map_err(|err| PageError(err.into()))?;
    let shared = Shared {
        forgettable: &forgettable,
        accounts: &account_views,
        binding: &binding,
        timezone: &timezone,
        csrf,
        can_manage,
        plaid_offered,
        refused_row: notice.as_ref().and_then(|notice| notice.refused_row),
    };
    let content = html! {
        h1 { "Connections" }
        @if let Some(notice) = &notice {
            p.notice role="alert" { (notice.message) }
        }
        @if connections.is_empty() {
            section.card {
                (empty_state(look, "No connections yet. Connect SimpleFin below to sync your bank accounts."))
            }
        }
        @for connection in &connections {
            @let may_remove = authorize(&actor, Action::RemoveConnection { owner: connection.owner }).is_ok();
            @let may_release = authorize(&actor, Action::ReleaseHeldCursor { owner: connection.owner }).is_ok();
            (connection_card(connection, Permissions { may_remove, may_release }, &shared))
        }
        @if can_manage {
            section.card {
                h2 { "Connect SimpleFin" }
                p {
                    "Create a setup token in the SimpleFin Bridge and paste it here. "
                    "coffer claims it once and keeps only the access it grants, encrypted."
                }
                form method="post" action="/connections/simplefin" {
                    (csrf_field(csrf))
                    label.field {
                        "Label"
                        input .input type="text" name="label" maxlength="100" required;
                    }
                    label.field {
                        "Setup token"
                        input .input type="password" name="setup_token" autocomplete="off"
                            required;
                    }
                    button.btn.btn-primary type="submit" { "Connect" }
                }
            }
            @if plaid_offered {
                section.card {
                    h2 { "Connect with Plaid" }
                    p { "Log in to your bank through Plaid. coffer keeps only the access it grants, encrypted." }
                    a.btn.btn-primary href="/connections/plaid/new" { "Connect with Plaid" }
                }
            }
        }
        (binding.offered_labels(&accounts))
    };
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Connections),
        narrow: false,
    };
    let markup = page(&frame, "Connections", &content);
    let Some(notice) = notice else {
        return Ok(markup.into_response());
    };
    let mut response = (notice.status, markup).into_response();
    if let Some(seconds) = notice.retry_after {
        set_retry_after(response.headers_mut(), seconds);
    }
    Ok(response)
}

/// `wait` in words, rounded up to the minute: "1 hour", "1 hour 5
/// minutes", "45 minutes".
fn wait_text(wait: SignedDuration) -> String {
    let minutes = u64::from(retry_seconds(wait)).div_ceil(60);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let unit = |count: u64, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    match (hours, minutes) {
        (0, minutes) => unit(minutes, "minute", "minutes"),
        (hours, 0) => unit(hours, "hour", "hours"),
        (hours, minutes) => format!(
            "{} {}",
            unit(hours, "hour", "hours"),
            unit(minutes, "minute", "minutes")
        ),
    }
}

/// Which kind of form failed: only the add and reconnect forms send a
/// setup token, so only they blame one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormKind {
    SetupToken,
    Action,
}

/// The notice a failed form action shows, or `None` when it deserves an
/// error page (a denial, a missing connection, a server failure).
fn notice_for(err: &SyncError, form: FormKind) -> Option<Notice> {
    let notice = match err {
        SyncError::Provider(provider @ ProviderError::Invalid(_))
            if form == FormKind::SetupToken =>
        {
            Notice::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "The setup token could not be used: {}.",
                    provider.record_text()
                ),
            )
        }
        SyncError::Provider(ProviderError::ReauthRequired) => Notice::new(
            StatusCode::CONFLICT,
            "SimpleFin refused the connection's access: reconnect it with a new setup token.",
        ),
        // Provider texts name the provider themselves.
        SyncError::Provider(provider) => Notice::new(
            StatusCode::BAD_GATEWAY,
            capitalized(&provider.record_text()),
        ),
        SyncError::RateLimited { retry_after } => Notice {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "The provider allows another sync of this connection in {}.",
                wait_text(*retry_after)
            ),
            retry_after: Some(retry_seconds(*retry_after)),
            refused_row: None,
        },
        SyncError::InProgress | SyncError::Conflict(_) | SyncError::CurrencyMismatch => {
            Notice::new(StatusCode::CONFLICT, capitalized(&err.to_string()))
        }
        SyncError::Invalid(_) => Notice::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            capitalized(&err.to_string()),
        ),
        _ => return None,
    };
    Some(notice)
}

/// `message` with its first letter upper case and a final full stop.
fn capitalized(message: &str) -> String {
    let mut chars = message.chars();
    let mut out: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

fn page_error(err: SyncError) -> PageError {
    PageError(Problem::from(err))
}

/// The page again with the notice for `err`, or its error page.
async fn refused(
    state: &AppState,
    session: &PageSession,
    err: SyncError,
    form: FormKind,
) -> Result<Response, PageError> {
    match notice_for(&err, form) {
        Some(notice) => render(state, session, Some(notice)).await,
        None => Err(page_error(err)),
    }
}

/// As [`refused`], for a bind form of the provider account `row`.
async fn refused_on_row(
    state: &AppState,
    session: &PageSession,
    err: SyncError,
    row: ProviderAccountId,
) -> Result<Response, PageError> {
    match notice_for(&err, FormKind::Action) {
        Some(notice) => render(state, session, Some(notice.on_row(row))).await,
        None => Err(page_error(err)),
    }
}

fn connection_id(raw: &str) -> Result<ConnectionId, PageError> {
    raw.parse().map_err(|_| PageError::not_found())
}

fn back() -> Response {
    Redirect::to("/connections").into_response()
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    setup_token: Option<SecretString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconnectForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    setup_token: Option<SecretString>,
}

/// The provider accounts the page showed as holding the cursor.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    accounts: String,
    #[serde(default)]
    records: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

/// A setup token that was sent and is not blank.
fn present(token: Option<SecretString>) -> Option<SecretString> {
    use secrecy::ExposeSecret as _;
    token.filter(|token| !token.expose_secret().trim().is_empty())
}

async fn add_simplefin(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<AddForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    authorize(&actor, Action::ManageConnections).map_err(AuthError::from)?;
    let Ok(label) = ConnectionLabel::parse(form.label.trim()) else {
        let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, LABEL_INVALID);
        return render(&state, &session, Some(notice)).await;
    };
    let Some(token) = present(form.setup_token) else {
        let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, TOKEN_MISSING);
        return render(&state, &session, Some(notice)).await;
    };
    match state
        .sync
        .create_connection(
            &actor,
            ProviderKind::SimpleFin,
            label,
            ConnectInput::SimpleFinSetupToken(token),
            None,
        )
        .await
    {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::SetupToken).await,
    }
}

async fn reconnect_simplefin(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<ReconnectForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = connection_id(&id)?;
    if let Err(err) = state.sync.authorize_reconnect(&actor, id).await {
        return refused(&state, &session, err, FormKind::SetupToken).await;
    }
    let Some(token) = present(form.setup_token) else {
        let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, TOKEN_MISSING);
        return render(&state, &session, Some(notice)).await;
    };
    match state
        .sync
        .reconnect(&actor, id, ConnectInput::SimpleFinSetupToken(token))
        .await
    {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::SetupToken).await,
    }
}

async fn sync(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = connection_id(&id)?;
    match state
        .sync
        .request_sync(Some(&actor), id, SyncTrigger::Manual)
        .await
    {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
    }
}

async fn remove(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = connection_id(&id)?;
    match state.sync.remove_connection(&actor, id).await {
        Ok(()) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
    }
}

async fn release_cursor(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<ReleaseForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = connection_id(&id)?;
    let shown: Result<Vec<ProviderAccountId>, _> = form
        .accounts
        .split(',')
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect();
    let (Ok(shown), Ok(records)) = (shown, form.records.parse::<u32>()) else {
        return refused(
            &state,
            &session,
            SyncError::Invalid(RELEASE_UNREADABLE),
            FormKind::Action,
        )
        .await;
    };
    match state
        .sync
        .release_held_cursor(&actor, id, &shown, records)
        .await
    {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use jiff::SignedDuration;

    use super::{FormKind, capitalized, notice_for, wait_text};
    use crate::sync::SyncError;
    use crate::sync::provider::ProviderError;

    #[test]
    fn waits_read_in_whole_minutes() {
        for (wait, text) in [
            (SignedDuration::from_hours(1), "1 hour"),
            (SignedDuration::from_mins(65), "1 hour 5 minutes"),
            (SignedDuration::from_mins(150), "2 hours 30 minutes"),
            (SignedDuration::from_secs(61), "2 minutes"),
            (SignedDuration::from_secs(1), "1 minute"),
            (SignedDuration::ZERO, "1 minute"),
        ] {
            assert_eq!(wait_text(wait), text);
        }
    }

    #[test]
    fn form_failures_become_notices_or_error_pages() {
        let limited = notice_for(
            &SyncError::RateLimited {
                retry_after: SignedDuration::from_mins(90),
            },
            FormKind::Action,
        )
        .unwrap();
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.retry_after, Some(5400));
        assert!(limited.message.contains("1 hour 30 minutes"));
        let invalid = notice_for(
            &SyncError::Provider(ProviderError::Invalid(
                "bad https://user:pw@h.example".into(),
            )),
            FormKind::SetupToken,
        )
        .unwrap();
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!invalid.message.contains("pw"));
        assert_eq!(
            notice_for(&SyncError::InProgress, FormKind::Action)
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
        assert!(notice_for(&SyncError::NotFound, FormKind::Action).is_none());
        assert_eq!(capitalized("a sync is queued"), "A sync is queued.");
    }

    /// Only the forms that send a setup token blame it; a provider text is
    /// shown once, without the provider's name in front of it again.
    #[test]
    fn provider_failures_read_per_form() {
        let refused = SyncError::Provider(ProviderError::Invalid("refused the request".into()));
        let on_remove = notice_for(&refused, FormKind::Action).unwrap();
        assert_eq!(on_remove.status, StatusCode::BAD_GATEWAY);
        assert!(!on_remove.message.contains("setup token"));
        assert_eq!(on_remove.message, "Refused the request.");
        let on_add = notice_for(&refused, FormKind::SetupToken).unwrap();
        assert!(
            on_add
                .message
                .starts_with("The setup token could not be used")
        );

        let unavailable = SyncError::Provider(ProviderError::Unavailable(
            "SimpleFin answered the claim with status 503".into(),
        ));
        assert_eq!(
            notice_for(&unavailable, FormKind::SetupToken)
                .unwrap()
                .message,
            "SimpleFin answered the claim with status 503."
        );
    }
}
