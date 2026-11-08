//! One rule's page: what it does, what running it now would change and what it would leave
//! alone (the dry run, which writes nothing), and the forms that run it, take back what it
//! filled, change it and delete it. Running it and taking back are in [`super::runs`].

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::ids::RuleId;
use coffer_domain::policy::{Action, authorize};
use maud::{Markup, html};
use serde::Deserialize;

use super::form::{self, Target};
use super::words::{amount_words, fills_words, left_alone_sentences, match_words, transactions};
use super::{Names, PAGE, refused};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::amounts::{day, money};
use crate::http::pages::layout::{Frame, Nav, csrf_field, page};
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::rules::RuleView;
use crate::ledger::rules::run::RuleDryRun;

pub(super) fn rule_id(text: &str) -> Result<RuleId, PageError> {
    text.parse().map_err(|_| PageError::not_found())
}

pub(super) async fn rule_page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let id = rule_id(&id)?;
    let rule = state.ledger.get_rule(&actor, id).await.map_err(refused)?;
    let rules = state.ledger.list_rules(&actor).await.map_err(refused)?;
    let place = Place::of(&rules.items, id);
    let trial = state
        .ledger
        .dry_run_rule(&actor, id, None)
        .await
        .map_err(refused)?;
    let names = Names::load(&state, &actor).await?;
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Rules),
        narrow: false,
    };
    // None for a viewer who couldn't change or run this rule, who sees what it would do and
    // no forms.
    let csrf = state
        .ledger
        .authorize_rule_input(&actor, rule.account)
        .await
        .is_ok()
        .then_some(viewer.csrf.as_str());
    let content = html! {
        p { a href=(PAGE) { "All rules" } }
        h1 { "Rule" }
        (summary(&rule, place, &names))
        (trial_card(&rule, &trial, &names, csrf))
        @if let Some(csrf) = csrf {
            (undo_card(&rule, csrf))
            section.card id="change" {
                h2 { "Change it" }
                p { "What it already filled stays as it is. Run it again afterwards to fill more." }
                (form::fields(Target::Change(rule.id), Some(&rule), &names, csrf))
            }
            (delete_card(&rule, csrf))
        }
    };
    Ok(page(&frame, "Rule", &content).into_response())
}

/// Where a rule comes in the order the rules run, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    rank: usize,
    of: usize,
}

impl Place {
    /// `id`'s place among `rules`, listed in the order they run; none if it isn't there.
    fn of(rules: &[RuleView], id: RuleId) -> Option<Self> {
        let index = rules.iter().position(|rule| rule.id == id)?;
        Some(Self {
            rank: index + 1,
            of: rules.len(),
        })
    }
}

/// The rule in a sentence, and what keeps it from filling anything for now.
fn summary(rule: &RuleView, place: Option<Place>, names: &Names) -> Markup {
    html! {
        section.card id="summary" {
            p {
                "When the bank description " (match_words(rule.match_kind, &rule.pattern)) ", on "
                (names.scope(rule.account, "every account"))
                @if let Some(amount) = rule.amount { ", for " (amount_words(amount)) }
                ", it " (fills_words(rule.category, rule.assignee, names)) "."
            }
            p {
                @if rule.enabled {
                    "It runs on new transactions as they arrive"
                    @if let Some(Place { rank, of }) = place {
                        ", number " (rank) " of " (of) " in the order"
                    }
                    "."
                } @else {
                    "It is off: it doesn't run on new transactions, but you can still try it and "
                    "run it here."
                }
            }
            @if rule.category_archived {
                p.notice {
                    "Its category is archived, so it files nothing until that is restored, and a "
                    "later rule may file instead."
                }
            }
            @if rule.assignee_disabled {
                p.notice {
                    "That member is disabled, so it assigns nobody until they are enabled, and a "
                    "later rule may assign instead."
                }
            }
        }
    }
}

/// What running it now would change, and the button that does it.
fn trial_card(rule: &RuleView, trial: &RuleDryRun, names: &Names, csrf: Option<&str>) -> Markup {
    let listed = u32::try_from(trial.changes.len()).unwrap_or(u32::MAX);
    let left = left_alone_sentences(trial.left_alone);
    html! {
        section.card id="dry-run" {
            h2 { "What running it now would change" }
            p {
                @if trial.would_change == 0 {
                    "Running it now would change nothing among the transactions already here that "
                    "you can see."
                } @else {
                    "Running it now would change " (transactions(trial.would_change))
                    " among those already here that you can see."
                    @if let (Some(category), true) = (rule.category, trial.would_categorize > 0) {
                        " It would file " (transactions(trial.would_categorize)) " under "
                        (names.category(category)) "."
                    }
                    @if let (Some(assignee), true) = (rule.assignee, trial.would_assign > 0) {
                        " It would assign " (transactions(trial.would_assign)) " to "
                        (names.member(assignee)) "."
                    }
                }
            }
            @if !trial.complete {
                p.fine {
                    "This looked at the oldest 20,000 open transactions only. Running it reaches "
                    "them all."
                }
            }
            @if !trial.changes.is_empty() {
                table.sums {
                    thead {
                        tr {
                            th scope="col" { "Day" }
                            th scope="col" { "Account" }
                            th scope="col" { "Bank description" }
                            th scope="col" { "Amount" }
                            th scope="col" { "It would" }
                        }
                    }
                    tbody {
                        @for change in &trial.changes {
                            tr {
                                td { (day(change.posted_on)) }
                                td { (names.account_link(change.account)) }
                                th scope="row" { (change.description) }
                                td { (money(change.amount)) }
                                td { (fills_words(change.category, change.assignee, names)) }
                            }
                        }
                    }
                }
                @if listed < trial.would_change {
                    p.fine { "These are the oldest " (listed) "." }
                }
            }
            @if !left.is_empty() {
                h3 { "What it matches and would leave alone" }
                ul.reasons {
                    @for sentence in &left { li { (sentence) } }
                }
            }
            @if let Some(csrf) = csrf {
                form method="post" action={ "/rules/" (rule.id) "/apply" } {
                    (csrf_field(csrf))
                    p {
                        "Running it fills only what nobody has set, on every transaction it "
                        "matches. It never marks anything reviewed and never changes what a "
                        "person chose. Running it again is safe: what is filled isn't filled again."
                    }
                    button.btn.btn-primary type="submit" { "Run it now" }
                }
            }
        }
    }
}

fn undo_card(rule: &RuleView, csrf: &str) -> Markup {
    html! {
        section.card id="undo" {
            h2 { "Take back what it filled" }
            p {
                "Empties each category and assignee this rule filled that nobody has changed "
                "since, and opens it again, as if no rule had run, so a corrected rule can fill "
                "it. Clearing one by hand would keep it empty instead."
            }
            p {
                "A transaction someone has reviewed is left for a person. The rule itself stays as "
                "it is: turn it off or correct it first, or it files new transactions again."
            }
            form method="post" action={ "/rules/" (rule.id) "/undo" } {
                (csrf_field(csrf))
                button.btn type="submit" { "Take back what it filled" }
            }
        }
    }
}

fn delete_card(rule: &RuleView, csrf: &str) -> Markup {
    html! {
        section.card id="delete" {
            h2 { "Delete it" }
            p {
                "What it filled stays in place, still naming it. To take that back, do it "
                "before deleting."
            }
            form method="post" action={ "/rules/" (rule.id) "/delete" } {
                (csrf_field(csrf))
                button.btn type="submit" { "Delete rule" }
            }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ButtonForm {
    #[serde(default)]
    pub(super) csrf: String,
}

/// Deletes the rule through the service the API uses, and goes back to the list.
pub(super) async fn delete(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ButtonForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let id = rule_id(&id)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    state
        .ledger
        .delete_rule(&actor, id)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(PAGE))
}
