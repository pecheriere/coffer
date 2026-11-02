//! Connections to data providers: each with its status, last sync and
//! errors, and the provider accounts it reports by name and binding. No
//! balance or transaction is shown (spec §6.4: pages carry no financial
//! data). Forms add a `SimpleFin` connection, reconnect one that needs it,
//! request a sync, remove a connection and bind a provider account to an
//! account (see [`bind`]); each needs the session's
//! anti-forgery token, and a setup token is never shown back. When Plaid is
//! configured it links to the Plaid Link pages.

mod bind;

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::policy::{Action, authorize};
use coffer_domain::sync::SyncTrigger;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use maud::{Markup, html};
use secrecy::SecretString;
use serde::Deserialize;

use super::account::viewer;
use super::layout::{Frame, Nav, csrf_field, empty_state, page};
use super::{PageError, PageForm, PageSession};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::problem::{Problem, set_retry_after};
use crate::sync::connections::{ConnectionState, ConnectionView, ProviderAccountView};
use crate::sync::provider::{ConnectInput, ProviderError, ProviderKind};
use crate::sync::{SyncError, retry_seconds};

const LABEL_INVALID: &str = "The label must be 1 to 100 characters without control characters.";
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
        .route(
            "/connections/provider-accounts/{id}/create-account",
            post(bind::create_account),
        )
        .route(
            "/connections/provider-accounts/{id}/bind",
            post(bind::link_account),
        )
}

/// A message about a form that did not go through, shown above the list
/// with the response's status.
struct Notice {
    status: StatusCode,
    message: String,
    /// Seconds until a retry may succeed, sent as `Retry-After`.
    retry_after: Option<u32>,
}

impl Notice {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
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
    let account_names: HashMap<AccountId, String> = accounts
        .into_iter()
        .map(|account| (account.id, account.name))
        .collect();
    let can_manage = authorize(&actor, Action::ManageConnections).is_ok();
    let csrf = &viewer.csrf;
    let plaid_offered = state.config.plaid.is_some();
    let shared = Shared {
        account_names: &account_names,
        binding: &binding,
        timezone: &state.config.timezone,
        csrf,
        can_manage,
        plaid_offered,
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
            (connection_card(connection, may_remove, &shared))
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

/// What every connection card shares.
struct Shared<'a> {
    account_names: &'a HashMap<AccountId, String>,
    binding: &'a bind::Binding,
    timezone: &'a TimeZone,
    csrf: &'a str,
    can_manage: bool,
    plaid_offered: bool,
}

fn connection_card(connection: &ConnectionView, may_remove: bool, shared: &Shared<'_>) -> Markup {
    let Shared {
        timezone,
        csrf,
        can_manage,
        plaid_offered,
        ..
    } = *shared;
    let plaid = connection.provider == ProviderKind::Plaid.as_str();
    let live = connection.status != ConnectionState::Removed;
    let id = connection.id;
    // The accounts that wait for a binding come first.
    let mut accounts: Vec<&ProviderAccountView> = connection.accounts.iter().collect();
    accounts.sort_by_key(|account| account.account.is_some());
    let waiting = live
        && shared.binding.can_bind()
        && accounts
            .first()
            .is_some_and(|first| first.account.is_none());
    html! {
        section.card {
            h2 { (connection.label) }
            div.item {
                div.item-main {
                    span.item-meta {
                        (connection.provider) " · Last synced "
                        (moment(connection.last_success_at, timezone))
                        " · Last attempt " (moment(connection.last_attempt_at, timezone))
                        @if let Some(next) = connection.next_sync_at {
                            " · Next sync " (moment(Some(next), timezone))
                        }
                    }
                    @if let Some(error) = &connection.last_error {
                        span.item-meta { "Error: " (error) }
                    }
                }
                (status_chip(connection.status))
                @if live && (can_manage || may_remove) {
                    div.item-actions {
                        @if can_manage {
                            form method="post" action={ "/connections/" (id) "/sync" } {
                                (csrf_field(csrf))
                                button.btn type="submit" { "Sync now" }
                            }
                        }
                        @if may_remove {
                            form method="post" action={ "/connections/" (id) "/remove" } {
                                (csrf_field(csrf))
                                button.btn type="submit" { "Remove" }
                            }
                        }
                    }
                }
            }
            @if may_remove && plaid && plaid_offered && connection.status == ConnectionState::ReauthRequired {
                h3 { "Reconnect" }
                p { "Plaid needs a new login to this institution; its accounts stay bound." }
                a.btn.btn-primary href={ "/connections/" (id) "/plaid/reconnect" } { "Reconnect" }
            }
            @if may_remove && !plaid && connection.status == ConnectionState::ReauthRequired {
                h3 { "Reconnect" }
                p { "SimpleFin no longer accepts this connection's access. Paste a new setup token to restore it; its accounts stay bound." }
                form method="post" action={ "/connections/" (id) "/simplefin/reconnect" } {
                    (csrf_field(csrf))
                    label.field {
                        "New setup token"
                        input .input type="password" name="setup_token" autocomplete="off"
                            required;
                    }
                    button.btn.btn-primary type="submit" { "Reconnect" }
                }
            }
            @if waiting {
                h3 { "Set up your accounts" }
                p {
                    "coffer found these bank accounts. Until each is bound to an account, "
                    "its transactions are held. Name each one and choose its kind, or link "
                    "it to an account you already have."
                }
            } @else {
                h3 { "Accounts" }
            }
            @if accounts.is_empty() {
                p.fine { "No accounts reported yet." }
            } @else {
                ul.items {
                    @for (index, account) in accounts.iter().enumerate() {
                        (account_item(connection, account, shared, waiting && index == 0))
                    }
                }
            }
        }
    }
}

fn account_item(
    connection: &ConnectionView,
    account: &ProviderAccountView,
    shared: &Shared<'_>,
    open: bool,
) -> Markup {
    html! {
        li.item {
            div.item-main {
                span.item-title { (account.name) }
                span.item-meta {
                    @if let Some(institution) = &account.institution {
                        (institution) " · "
                    }
                    (account.currency)
                }
                @if let Some(error) = &account.error {
                    span.item-meta { "Error: " (error) }
                }
            }
            @match account.account {
                Some(bound) => {
                    span.chip {
                        "Bound to "
                        (shared.account_names.get(&bound).map_or("an account", String::as_str))
                    }
                }
                None => span.chip.chip-quiet { "Not bound" },
            }
            (shared.binding.forms(connection, account, shared.csrf, open))
        }
    }
}

fn status_chip(status: ConnectionState) -> Markup {
    let text = match status {
        ConnectionState::Active => "active",
        ConnectionState::Error => "error",
        ConnectionState::ReauthRequired => "reconnect needed",
        ConnectionState::Removed => "removed",
    };
    html! {
        @if status == ConnectionState::Active {
            span.chip { (text) }
        } @else {
            span.chip.chip-quiet { (text) }
        }
    }
}

/// `at` in the household's time zone, to the minute; "never" without one.
fn moment(at: Option<Timestamp>, timezone: &TimeZone) -> String {
    at.map_or_else(
        || "never".to_owned(),
        |at| {
            at.to_zoned(timezone.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
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
