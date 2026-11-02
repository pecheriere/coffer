//! Forgetting a Plaid connection whose sealed token cannot be opened: the card's section
//! with its form, and the handler behind it.

use axum::extract::{Path, State};
use axum::response::Response;
use coffer_domain::ids::ConnectionId;
use maud::{Markup, html};
use serde::Deserialize;

use super::{FormKind, back, connection_id, refused};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForgetForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    confirmation: String,
}

/// The section of a connection's card that forgets it, for a connection
/// whose removal is refused because coffer cannot give it up.
pub(super) fn section(id: ConnectionId, label: &str, csrf: &str) -> Markup {
    html! {
        h3 { "Forget this connection" }
        p {
            "coffer cannot open this connection's sealed token, so it cannot remove the "
            "Item at Plaid and cannot remove this connection."
        }
        p {
            strong {
                "If this could be a wrong key or a restored backup, fix that first: "
                "forgetting can't be undone, and removal then works normally."
            }
        }
        p {
            "Forgetting marks this connection removed and erases its sealed token here; "
            "its accounts and transactions stay. "
            strong {
                "coffer can't end billing for this Item, because that needs the token. "
                "Contact Plaid, or check your Plaid dashboard, to end it."
            }
        }
        form method="post" action={ "/connections/" (id) "/forget" } {
            (csrf_field(csrf))
            label.field {
                "Type the connection's label, " strong { (label) } ", to confirm"
                input .input type="text" name="confirmation" autocomplete="off" required;
            }
            button.btn type="submit" { "Forget without giving up" }
        }
    }
}

pub(super) async fn forget(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<ForgetForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = connection_id(&id)?;
    match state
        .sync
        .forget_connection(&actor, id, &form.confirmation)
        .await
    {
        Ok(()) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
    }
}
