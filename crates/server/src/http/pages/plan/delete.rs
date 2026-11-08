//! Deleting a planned item: a page that says what deleting does and asks to confirm it, and
//! the form behind it. Nothing is deleted from the plan page itself.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::plan::PlanSchedule;
use maud::{Markup, html};
use serde::Deserialize;

use super::edit::{item_id, may_write};
use super::{PAGE, back_to, frame, refused};
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::layout::{csrf_field, page};
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::plan_items::PlannedItemView;

pub(super) async fn confirm_page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    may_write(&state, &session).await?;
    let item = state
        .ledger
        .get_planned_item(&session.actor(), item_id(&id)?)
        .await
        .map_err(refused)?;
    let viewer = viewer(&state, &session).await;
    let frame = frame(&state, &session, &viewer).await;
    let title = format!("Delete {}?", item.name);
    Ok(page(&frame, &title, &confirm_content(&item, &viewer.csrf)).into_response())
}

fn confirm_content(item: &PlannedItemView, csrf: &str) -> Markup {
    html! {
        h1 { "Delete " (item.name) "?" }
        section.card id="confirm-delete" {
            p {
                "This deletes " (item.name) " for good, with the months settled for it. The "
                "forecast stops counting it, and the audit log keeps what it was. Transactions "
                "stay as they are."
            }
            @if matches!(item.schedule, PlanSchedule::Goal { .. }) {
                p {
                    "A goal with spending linked to it can't be deleted: mark it completed "
                    "instead, so it needs no more money."
                }
            }
            @if item.active {
                p { "To stop counting it for a while instead, pause it on the plan page." }
            }
            form method="post" action={ (PAGE) "/" (item.id) "/delete" } {
                (csrf_field(csrf))
                button.btn.btn-primary type="submit" { "Delete " (item.name) }
            }
            p { a href=(back_to(item.id)) { "Keep it" } }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteForm {
    #[serde(default)]
    csrf: String,
}

pub(super) async fn delete(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<DeleteForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    state
        .ledger
        .delete_planned_item(&actor, item_id(&id)?)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(PAGE))
}
