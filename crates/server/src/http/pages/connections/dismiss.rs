//! Leaving a provider account unbound on purpose: the form that dismisses it, the one that takes
//! that back, and the handlers behind them. The forms ask the sync service, which decides and
//! audits; the page only leaves out the forms that the actor's role could not use.

use axum::extract::{Path, State};
use axum::response::Response;
use coffer_domain::ids::ProviderAccountId;
use maud::{Markup, html};

use super::{CsrfForm, back, refused_on_row};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::sync::connections::{ConnectionState, ConnectionView, ProviderAccountView};

/// What dismissing does, said before the button is pressed.
const DISMISS_FINE: &str = "Dismissing says this account is meant to stay unbound, such as one \
    held for someone else. The forecast stops warning about it and it stops counting as an \
    account to set up. Its records stay held, and you can bind it or undo this any time. \
    The audit log records it.";
const DISMISSED_FINE: &str = "You dismissed this account: the forecast doesn't warn about it. \
    Its records stay held until you bind it, which ends the dismissal.";

/// The form that dismisses an unbound provider account, or the one that undoes it, for a live
/// connection; nothing for a bound one or a removed connection, or for who may not use them.
pub(super) fn forms(
    connection: &ConnectionView,
    account: &ProviderAccountView,
    csrf: &str,
    allowed: bool,
) -> Markup {
    if !allowed || account.account.is_some() || connection.status == ConnectionState::Removed {
        return html! {};
    }
    let id = account.id;
    html! {
        @if account.dismissed_at.is_some() {
            p.fine { (DISMISSED_FINE) }
            form method="post" action={ "/connections/provider-accounts/" (id) "/restore" } {
                (csrf_field(csrf))
                button.btn type="submit" { "Undo dismiss" }
            }
        } @else {
            form method="post" action={ "/connections/provider-accounts/" (id) "/dismiss" } {
                (csrf_field(csrf))
                p.fine { (DISMISS_FINE) }
                button.btn type="submit" { "Dismiss" }
            }
        }
    }
}

fn provider_account_id(raw: &str) -> Result<ProviderAccountId, PageError> {
    raw.parse().map_err(|_| PageError::not_found())
}

pub(super) async fn dismiss(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = provider_account_id(&id)?;
    match state.sync.dismiss_provider_account(&actor, id).await {
        Ok(_) => Ok(back()),
        Err(err) => refused_on_row(&state, &session, err, id).await,
    }
}

pub(super) async fn restore(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CsrfForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = provider_account_id(&id)?;
    match state.sync.restore_provider_account(&actor, id).await {
        Ok(_) => Ok(back()),
        Err(err) => refused_on_row(&state, &session, err, id).await,
    }
}
