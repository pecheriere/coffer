//! Forgetting a Plaid connection coffer cannot give up: the card's section
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
            "coffer cannot reach Plaid for this connection: Plaid is not configured on "
            "this server, or the sealed token no longer opens, for example because the "
            "server key is lost. Until then it cannot be removed."
        }
        p {
            strong {
                "The Item keeps billing at Plaid until it is removed in Plaid's dashboard, "
                "and coffer cannot do that for it."
            }
            " Forgetting marks this connection removed and erases its sealed token here; "
            "its accounts and transactions stay. Remove the Item in Plaid's dashboard too."
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
