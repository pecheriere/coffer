//! The plan page: the household's planned items (bills, budgets, income, one-offs and goals)
//! in words, and the forms that add, change, pause and delete them. Every read and write goes
//! through the service the API uses, so who may do what, what an item may hold and the audit
//! stay in one place; the page only leaves out the forms a viewer could not use.

mod delete;
mod edit;
mod form;
mod words;

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coffer_domain::forecast::LineSource;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use maud::{Markup, html};

use super::account::viewer;
use super::amounts::magnitude;
use super::layout::{Frame, Nav, Viewer, csrf_field, empty_state, page};
use super::{PageError, PageSession, refused};
use crate::http::AppState;
use crate::ledger::accounts::AccountView;
use crate::ledger::categories::CategoryGroupView;
use crate::ledger::forecast::ForecastOptions;
use crate::ledger::plan_items::PlannedItemView;
use crate::themes::Look;
use form::Choices;
use words::{direction_word, schedule_words, type_word};

const PAGE: &str = "/plan";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PAGE, get(plan_page).post(edit::create))
        .route("/plan/{id}", get(edit::edit_page).post(edit::update))
        .route("/plan/{id}/active", post(edit::set_active))
        .route(
            "/plan/{id}/delete",
            get(delete::confirm_page).post(delete::delete),
        )
}

/// Where a change sends the browser back to.
fn back_to(id: PlannedItemId) -> String {
    format!("{PAGE}#item-{id}")
}

/// What the forms choose from, read through the ledger as the viewer.
struct Loaded {
    groups: Vec<CategoryGroupView>,
    accounts: Vec<AccountView>,
    currency: Option<Currency>,
}

impl Loaded {
    async fn read(state: &AppState, actor: &Actor) -> Result<Self, PageError> {
        let groups = state
            .ledger
            .list_categories(actor)
            .await
            .map_err(refused)?
            .items;
        let accounts = state
            .ledger
            .list_accounts(actor, true)
            .await
            .map_err(refused)?;
        let display = state
            .ledger
            .household_settings(actor)
            .await
            .map_err(refused)?
            .display_currency;
        // A new item starts in the household's display currency, or its first account's.
        let currency = display.or_else(|| {
            accounts
                .iter()
                .find(|account| !account.closed)
                .map(|account| account.currency)
        });
        Ok(Self {
            groups,
            accounts,
            currency,
        })
    }

    fn choices(&self) -> Choices<'_> {
        Choices {
            groups: &self.groups,
            accounts: &self.accounts,
            currency: self.currency,
        }
    }
}

/// The frame of every plan page.
async fn frame<'a>(state: &AppState, session: &PageSession, viewer: &'a Viewer) -> Frame<'a> {
    Frame {
        look: session.look(state).await,
        viewer: Some(viewer),
        current: Some(Nav::Money),
        narrow: false,
    }
}

/// The anti-forgery token when the viewer may change the plan, else nothing: they see the
/// items and no forms.
async fn csrf_if_writer<'a>(
    state: &AppState,
    actor: &Actor,
    viewer: &'a Viewer,
) -> Option<&'a str> {
    state
        .ledger
        .authorize_plan_write(actor)
        .await
        .is_ok()
        .then_some(viewer.csrf.as_str())
}

/// What was spent from each goal, from the forecast's goal lines. Only a goal that still needs
/// money in the forecast's window has a line, so one that is complete, set aside or past due
/// shows no figure. The figures are a courtesy: a forecast that can't be read costs them, and
/// the page reads without them.
async fn goal_spent(state: &AppState, actor: &Actor) -> HashMap<PlannedItemId, Money> {
    match state
        .ledger
        .forecast(actor, ForecastOptions::default())
        .await
    {
        Ok(forecast) => forecast
            .items
            .iter()
            .filter_map(|line| match (line.source, line.goal) {
                (LineSource::PlannedItem { id }, Some(goal)) => Some((id, goal.spent)),
                _ => None,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "could not read the plan's goal figures");
            HashMap::new()
        }
    }
}

async fn plan_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let mut items = state
        .ledger
        .list_planned_items(&actor, true)
        .await
        .map_err(refused)?;
    // Paused items last, each part in the order they were added.
    items.sort_by_key(|item| !item.active);
    let loaded = Loaded::read(&state, &actor).await?;
    let goals = goal_spent(&state, &actor).await;
    let viewer = viewer(&state, &session).await;
    let frame = frame(&state, &session, &viewer).await;
    let csrf = csrf_if_writer(&state, &actor, &viewer).await;
    let names = Names::new(&loaded);
    let content = content(frame.look, &items, &names, &goals, &loaded.choices(), csrf);
    Ok(page(&frame, "Plan", &content).into_response())
}

/// The names of what an item points at.
struct Names<'a> {
    categories: HashMap<CategoryId, &'a str>,
    accounts: HashMap<AccountId, &'a str>,
}

impl<'a> Names<'a> {
    fn new(loaded: &'a Loaded) -> Self {
        let categories = loaded
            .groups
            .iter()
            .flat_map(|group| &group.categories)
            .map(|category| (category.id, category.name.as_str()))
            .collect();
        let accounts = loaded
            .accounts
            .iter()
            .map(|account| (account.id, account.name.as_str()))
            .collect();
        Self {
            categories,
            accounts,
        }
    }
}

fn content(
    look: Look,
    items: &[PlannedItemView],
    names: &Names<'_>,
    goals: &HashMap<PlannedItemId, Money>,
    choices: &Choices<'_>,
    csrf: Option<&str>,
) -> Markup {
    html! {
        h1 { "Plan" }
        p {
            "The bills, budgets, income, one-offs and goals the forecast counts on. "
            a href="/forecast" { "See the forecast" }
        }
        section.card id="items" {
            @if items.is_empty() {
                (empty_state(look, "Nothing planned yet. Add the bills and budgets you expect, so the forecast has them."))
            } @else {
                ul.items {
                    @for item in items {
                        (item_row(item, names, goals.get(&item.id), csrf))
                    }
                }
            }
        }
        @if let Some(csrf) = csrf {
            section.card id="add-item" {
                h2 { "Add a planned item" }
                @if choices.has_usable_category() {
                    form method="post" action=(PAGE) {
                        (csrf_field(csrf))
                        (form::fields(choices, None))
                        button.btn.btn-primary type="submit" { "Add to the plan" }
                    }
                } @else {
                    p {
                        "Every planned item needs a category for spending or income. "
                        a href="/categories" { "Add one on the categories page" }
                        " first."
                    }
                }
            }
        }
    }
}

fn item_row(
    item: &PlannedItemView,
    names: &Names<'_>,
    goal_spent: Option<&Money>,
    csrf: Option<&str>,
) -> Markup {
    let category = names
        .categories
        .get(&item.category)
        .copied()
        .unwrap_or("a category");
    let account = item
        .account_hint
        .and_then(|account| names.accounts.get(&account).copied());
    html! {
        li.item id={ "item-" (item.id) } {
            div.item-main {
                span.item-title { (item.name) }
                span.item-meta {
                    (type_word(&item.schedule, item.direction, item.spending)) " · "
                    (magnitude(item.amount)) " " (direction_word(item.direction)) " · "
                    (category)
                    @if let Some(account) = account { " · on " (account) }
                }
                span.item-meta { (schedule_words(&item.schedule)) }
                @if let Some(spent) = goal_spent {
                    span.item-meta { "Spent from it so far " (magnitude(*spent)) }
                }
                @if let Some(notes) = &item.notes {
                    span.item-meta { (notes) }
                }
            }
            @if !item.active {
                span.chip.chip-quiet { "Paused" }
            }
            @if let Some(csrf) = csrf {
                div.item-actions {
                    a.btn href={ (PAGE) "/" (item.id) } { "Change" }
                    form method="post" action={ (PAGE) "/" (item.id) "/active" } {
                        (csrf_field(csrf))
                        @if item.active {
                            input type="hidden" name="active" value="no";
                            button.btn type="submit" { "Pause" }
                        } @else {
                            input type="hidden" name="active" value="yes";
                            button.btn type="submit" { "Resume" }
                        }
                    }
                    a.btn href={ (PAGE) "/" (item.id) "/delete" } { "Delete" }
                }
            }
        }
    }
}
