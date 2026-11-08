//! The handlers behind the categories page's forms. Each checks the policy before reading the
//! form, so a viewer who may not change categories is refused whatever they send, then hands
//! the change to the ledger, which validates and audits it as for the API. Every refusal is
//! fixed text that never echoes the input.

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{CategoryGroupId, CategoryId};
use coffer_domain::transactions::CategoryKind;
use serde::Deserialize;

use super::{KIND_LOCKED, PAGE};
use crate::http::AppState;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::categories::{
    CategoryGroupInput, CategoryGroupPatch, CategoryInput, CategoryPatch, KIND_IN_USE,
};

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateGroupForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    name: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GroupForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    position: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateCategoryForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CategoryForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    position: String,
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    archived: String,
}

fn refused(err: LedgerError) -> PageError {
    PageError(Problem::from(err))
}

/// The policy check every form makes before it is read.
async fn may_write(state: &AppState, session: &PageSession) -> Result<(), PageError> {
    state
        .ledger
        .authorize_category_write(&session.actor())
        .await
        .map_err(refused)
}

/// The anti-forgery check, then the form itself.
fn checked<T>(
    state: &AppState,
    session: &PageSession,
    form: Result<PageForm<T>, PageError>,
    csrf: impl Fn(&T) -> &str,
) -> Result<(Actor, T), PageError> {
    let PageForm(form) = form?;
    let actor = session.verify(state, csrf(&form))?;
    Ok((actor, form))
}

fn kind_of(text: &str) -> Result<CategoryKind, PageError> {
    text.parse()
        .map_err(|_| PageError::invalid("not a kind the form offers"))
}

fn group_of(text: &str) -> Result<CategoryGroupId, PageError> {
    text.parse()
        .map_err(|_| PageError::invalid("not a group the form offers"))
}

/// A whole number; the ledger says how large it may be.
fn position_of(text: &str) -> Result<u32, PageError> {
    text.trim()
        .parse()
        .map_err(|_| PageError::invalid("order is a whole number from 0 to 10000"))
}

fn archived_of(text: &str) -> Result<bool, PageError> {
    match text {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(PageError::invalid("not an answer the form offers")),
    }
}

fn group_id(path: &str) -> Result<CategoryGroupId, PageError> {
    path.parse().map_err(|_| PageError::not_found())
}

fn category_id(path: &str) -> Result<CategoryId, PageError> {
    path.parse().map_err(|_| PageError::not_found())
}

fn back_to(group: CategoryGroupId) -> Redirect {
    Redirect::to(&format!("{PAGE}#group-{group}"))
}

pub(super) async fn create_group(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<CreateGroupForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let input = CategoryGroupInput {
        name: form.name,
        position: None,
    };
    let group = state
        .ledger
        .create_category_group(&actor, input, None)
        .await
        .map_err(refused)?;
    Ok(back_to(group.id))
}

pub(super) async fn update_group(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<GroupForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let id = group_id(&id)?;
    let patch = CategoryGroupPatch {
        name: Some(form.name),
        position: Some(position_of(&form.position)?),
        archived: None,
    };
    state
        .ledger
        .update_category_group(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(back_to(id))
}

pub(super) async fn archive_group(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ArchiveForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let id = group_id(&id)?;
    let patch = CategoryGroupPatch {
        archived: Some(archived_of(&form.archived)?),
        ..CategoryGroupPatch::default()
    };
    state
        .ledger
        .update_category_group(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(back_to(id))
}

pub(super) async fn create_category(
    State(state): State<AppState>,
    session: PageSession,
    form: Result<PageForm<CreateCategoryForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let input = CategoryInput {
        group: group_of(&form.group)?,
        name: form.name,
        kind: kind_of(&form.kind)?,
        position: None,
    };
    let category = state
        .ledger
        .create_category(&actor, input, None)
        .await
        .map_err(refused)?;
    Ok(back_to(category.group))
}

pub(super) async fn update_category(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<CategoryForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let id = category_id(&id)?;
    let patch = CategoryPatch {
        group: Some(group_of(&form.group)?),
        name: Some(form.name),
        kind: Some(kind_of(&form.kind)?),
        position: Some(position_of(&form.position)?),
        archived: None,
    };
    let category = state
        .ledger
        .update_category(&actor, id, patch)
        .await
        .map_err(|err| match err {
            // The same words as the hint on the form.
            LedgerError::Invalid(KIND_IN_USE) => PageError::invalid(KIND_LOCKED),
            other => refused(other),
        })?;
    Ok(back_to(category.group))
}

pub(super) async fn archive_category(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ArchiveForm>, PageError>,
) -> Result<Redirect, PageError> {
    may_write(&state, &session).await?;
    let (actor, form) = checked(&state, &session, form, |form| &form.csrf)?;
    let id = category_id(&id)?;
    let patch = CategoryPatch {
        archived: Some(archived_of(&form.archived)?),
        ..CategoryPatch::default()
    };
    let category = state
        .ledger
        .update_category(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(back_to(category.group))
}
