//! The categories page: the household's category groups and their categories, and the forms
//! that create, rename, move, reorder and archive them. Every read and write goes through the
//! service the API uses, so who may do what and the audit stay in one place; the page only
//! leaves out the forms a viewer could not use.

mod forms;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coffer_domain::transactions::CategoryKind;
use maud::{Markup, html};

use super::account::viewer;
use super::layout::{Frame, Nav, csrf_field, empty_state, page};
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::categories::{CategoryGroupView, CategoryTree, CategoryView};
use crate::themes::Look;

const PAGE: &str = "/categories";
const GROUPS: &str = "/category-groups";
/// When a category's kind is locked: the hint on its form, and the refusal when a change is
/// tried anyway.
const KIND_LOCKED: &str =
    "Its kind can't change while a planned item uses it or spending in it is linked to a goal.";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PAGE, get(categories_page).post(forms::create_category))
        .route("/categories/{id}", post(forms::update_category))
        .route("/categories/{id}/archive", post(forms::archive_category))
        .route(GROUPS, post(forms::create_group))
        .route("/category-groups/{id}", post(forms::update_group))
        .route("/category-groups/{id}/archive", post(forms::archive_group))
}

async fn categories_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let tree = state
        .ledger
        .list_categories(&actor)
        .await
        .map_err(|err| PageError(Problem::from(err)))?;
    let viewer = viewer(&state, &session).await;
    let look = session.look(&state).await;
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Categories),
        narrow: false,
    };
    // None for a viewer who couldn't change them, who sees the list and no forms. Any other
    // failure fails the page rather than passing for a refusal.
    let csrf = match state.ledger.authorize_category_write(&actor).await {
        Ok(()) => Some(viewer.csrf.as_str()),
        Err(LedgerError::Denied(_)) => None,
        Err(err) => return Err(PageError(Problem::from(err))),
    };
    Ok(page(&frame, "Categories", &content(look, &tree, csrf)).into_response())
}

fn content(look: Look, tree: &CategoryTree, csrf: Option<&str>) -> Markup {
    html! {
        h1 { "Categories" }
        p {
            "A category says what money was for: spending, income, or a transfer between your "
            "own accounts. Every planned item needs one, and each sits in a group, such as Home "
            "or Food."
        }
        @if tree.items.is_empty() {
            section.card {
                (empty_state(look, "No categories yet. Start with a group, then add categories to it."))
            }
        }
        @for group in &tree.items {
            (group_card(group, &tree.items, csrf))
        }
        @if let Some(csrf) = csrf {
            @if !tree.items.is_empty() {
                (add_category(&tree.items, csrf))
            }
            (add_group(csrf))
        }
    }
}

/// What a kind is called on a page.
fn kind_label(kind: CategoryKind) -> &'static str {
    match kind {
        CategoryKind::Expense => "Spending",
        CategoryKind::Income => "Income",
        CategoryKind::Transfer => "Transfer between your own accounts",
    }
}

const KINDS: [CategoryKind; 3] = [
    CategoryKind::Expense,
    CategoryKind::Income,
    CategoryKind::Transfer,
];

fn kind_field(chosen: Option<CategoryKind>) -> Markup {
    html! {
        label.field {
            "Kind"
            select.input name="kind" required {
                @if chosen.is_none() {
                    option value="" selected { "Choose a kind" }
                }
                @for kind in KINDS {
                    option value=(kind.as_str()) selected[chosen == Some(kind)] { (kind_label(kind)) }
                }
            }
        }
    }
}

fn group_field(
    label: &str,
    groups: &[CategoryGroupView],
    chosen: Option<&CategoryGroupView>,
) -> Markup {
    html! {
        label.field {
            (label)
            select.input name="group" required {
                @if chosen.is_none() {
                    option value="" selected { "Choose a group" }
                }
                @for group in groups {
                    option value=(group.id) selected[chosen.is_some_and(|chosen| chosen.id == group.id)] {
                        (group.name)
                        @if group.archived { " (archived)" }
                    }
                }
            }
        }
    }
}

fn position_field(position: u32) -> Markup {
    html! {
        label.field {
            "Order, from 0 to 10000: lower comes first"
            input.input type="number" name="position" min="0" max="10000" step="1"
                value=(position) required;
        }
    }
}

fn name_field(label: &str, value: Option<&str>) -> Markup {
    html! {
        label.field {
            (label)
            input.input type="text" name="name" required maxlength="100" autocomplete="off"
                value=[value];
        }
    }
}

/// The form that archives what `action` names, or restores it.
fn archive_form(action: &str, archived: bool, csrf: &str) -> Markup {
    html! {
        form method="post" action=(action) {
            (csrf_field(csrf))
            @if archived {
                input type="hidden" name="archived" value="no";
                button.btn type="submit" { "Restore" }
            } @else {
                input type="hidden" name="archived" value="yes";
                button.btn type="submit" { "Archive" }
            }
        }
    }
}

fn group_card(
    group: &CategoryGroupView,
    groups: &[CategoryGroupView],
    csrf: Option<&str>,
) -> Markup {
    html! {
        section.card id={ "group-" (group.id) } {
            h2 {
                (group.name)
                @if group.archived { " " span.chip.chip-quiet { "Archived" } }
            }
            @if group.archived {
                p.fine {
                    "Every category in an archived group counts as archived: it takes no new "
                    "transactions, and those it has stay."
                }
            }
            @if group.categories.is_empty() {
                p.fine { "No categories in this group yet." }
            } @else {
                ul.items {
                    @for category in &group.categories {
                        (category_item(category, group, groups, csrf))
                    }
                }
            }
            @if let Some(csrf) = csrf {
                details.item-detail {
                    summary { "Change this group" }
                    form method="post" action={ (GROUPS) "/" (group.id) } {
                        (csrf_field(csrf))
                        (name_field("Name", Some(&group.name)))
                        (position_field(group.position))
                        button.btn.btn-primary type="submit" { "Save group" }
                    }
                    p.fine {
                        @if group.archived {
                            "Restoring the group restores its categories, except those archived "
                            "on their own."
                        } @else {
                            "Archiving the group archives its categories with it. Their "
                            "transactions and planned items stay as they are."
                        }
                    }
                    (archive_form(&format!("{GROUPS}/{}/archive", group.id), group.archived, csrf))
                }
            }
        }
    }
}

fn category_item(
    category: &CategoryView,
    group: &CategoryGroupView,
    groups: &[CategoryGroupView],
    csrf: Option<&str>,
) -> Markup {
    let action = format!("{PAGE}/{}", category.id);
    html! {
        li.item {
            div.item-main {
                span.item-title { (category.name) }
                span.item-meta { (kind_label(category.kind)) }
            }
            @if category.archived {
                span.chip.chip-quiet { "Archived" }
            }
            @if let Some(csrf) = csrf {
                div.item-actions {
                    (archive_form(&format!("{action}/archive"), category.archived, csrf))
                }
                details.item-detail {
                    summary { "Rename, move or reorder" }
                    form method="post" action=(action) {
                        (csrf_field(csrf))
                        (name_field("Name", Some(&category.name)))
                        (kind_field(Some(category.kind)))
                        (group_field("Group", groups, Some(group)))
                        (position_field(category.position))
                        p.fine {
                            (KIND_LOCKED)
                            " An archived category takes no new transactions; those it has stay."
                        }
                        button.btn.btn-primary type="submit" { "Save category" }
                    }
                }
            }
        }
    }
}

fn add_category(groups: &[CategoryGroupView], csrf: &str) -> Markup {
    html! {
        section.card id="add-category" {
            h2 { "Add a category" }
            form method="post" action=(PAGE) {
                (csrf_field(csrf))
                (group_field("Group", groups, None))
                (name_field("Name, such as Groceries", None))
                (kind_field(None))
                button.btn.btn-primary type="submit" { "Add category" }
            }
        }
    }
}

fn add_group(csrf: &str) -> Markup {
    html! {
        section.card id="add-group" {
            h2 { "Add a group" }
            p.fine { "A new group comes after the others." }
            form method="post" action=(GROUPS) {
                (csrf_field(csrf))
                (name_field("Name, such as Home", None))
                button.btn.btn-primary type="submit" { "Add group" }
            }
        }
    }
}
