//! Adding a planned item, the page that changes one, and pausing or resuming it. Each handler
//! checks the policy before reading the form, so a viewer who may not change the plan is
//! refused whatever they send, then hands the item to the ledger, which validates and audits
//! it as for the API.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::ids::PlannedItemId;
use maud::{Markup, html};
use serde::Deserialize;

use super::form::{self, Choices, ItemForm};
use super::{Loaded, PAGE, back_to, frame, refused};
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::layout::{csrf_field, page};
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::plan_items::{PlannedItemPatch, PlannedItemView};

/// The policy check every form makes before it is read.
pub(super) async fn may_write(state: &AppState, session: &PageSession) -> Result<(), PageError> {
    state
        .ledger
        .authorize_plan_write(&session.actor())
        .await
        .map_err(refused)
}

/// An item id from the path; one that can't be read is missing.
pub(super) fn item_id(path: &str) -> Result<PlannedItemId, PageError> {
    path.parse().map_err(|_| PageError::not_found())
}

pub(super) async fn create(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<ItemForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let input = form.fields()?.into_input();
    let item = state
        .ledger
        .create_planned_item(&actor, input, None)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&back_to(item.id)))
}

pub(super) async fn edit_page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    may_write(&state, &session).await?;
    let actor = session.actor();
    let item = state
        .ledger
        .get_planned_item(&actor, item_id(&id)?)
        .await
        .map_err(refused)?;
    let loaded = Loaded::read(&state, &actor).await?;
    let viewer = viewer(&state, &session).await;
    let frame = frame(&state, &session, &viewer).await;
    let title = format!("Change {}", item.name);
    let content = edit_content(&item, &loaded.choices(), &viewer.csrf);
    Ok(page(&frame, &title, &content).into_response())
}

fn edit_content(item: &PlannedItemView, choices: &Choices<'_>, csrf: &str) -> Markup {
    html! {
        h1 { "Change " (item.name) }
        section.card id="change-item" {
            p {
                "Saving changes only what differs. A new amount or schedule undoes what was "
                "settled for this month and later; earlier months stay as they were."
            }
            form method="post" action={ (PAGE) "/" (item.id) } {
                (csrf_field(csrf))
                (form::fields(choices, Some(item)))
                button.btn.btn-primary type="submit" { "Save changes" }
            }
            p { a href=(back_to(item.id)) { "Back to the plan without saving" } }
        }
    }
}

pub(super) async fn update(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ItemForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let id = item_id(&id)?;
    let patch = form.fields()?.into_patch();
    state
        .ledger
        .update_planned_item(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&back_to(id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    active: String,
}

pub(super) async fn set_active(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ActiveForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let id = item_id(&id)?;
    let active = match form.active.as_str() {
        "yes" => true,
        "no" => false,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    let patch = PlannedItemPatch {
        active: Some(active),
        ..PlannedItemPatch::default()
    };
    state
        .ledger
        .update_planned_item(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&back_to(id)))
}
