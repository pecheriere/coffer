//! Running a rule by hand and taking back what it filled, through the services the API uses.
//! Each answers 303 to a page saying what it did, so reloading that page, or going back to it,
//! never runs or takes back again: a later reload of a take-back would otherwise empty what a
//! corrected rule had filled since. That page's address holds only the counts the ledger
//! answered, read back strictly as whole numbers.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::ids::RuleId;
use coffer_domain::policy::{Action, authorize};
use maud::html;
use serde::{Deserialize, Serialize};

use super::detail::{ButtonForm, rule_id};
use super::words::{applied_sentences, left_alone_sentences, undone_sentences};
use super::{Names, refused};
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::layout::{Frame, Nav, page};
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::rules::run::{RuleApplied, RuleLeftAlone};
use crate::ledger::rules::undo::RuleUndone;

/// What running a rule did, as its results page's address holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ran {
    changed: u32,
    categorized: u32,
    assigned: u32,
    reviewed: u32,
    split: u32,
    transfer_leg: u32,
    income_on_outflow: u32,
    zero_amount: u32,
    earlier_rule: u32,
}

impl From<RuleApplied> for Ran {
    fn from(done: RuleApplied) -> Self {
        let RuleLeftAlone {
            reviewed,
            split,
            transfer_leg,
            income_on_outflow,
            zero_amount,
            earlier_rule,
        } = done.left_alone;
        Self {
            changed: done.changed,
            categorized: done.categorized,
            assigned: done.assigned,
            reviewed,
            split,
            transfer_leg,
            income_on_outflow,
            zero_amount,
            earlier_rule,
        }
    }
}

impl From<Ran> for RuleApplied {
    fn from(ran: Ran) -> Self {
        Self {
            changed: ran.changed,
            categorized: ran.categorized,
            assigned: ran.assigned,
            left_alone: RuleLeftAlone {
                reviewed: ran.reviewed,
                split: ran.split,
                transfer_leg: ran.transfer_leg,
                income_on_outflow: ran.income_on_outflow,
                zero_amount: ran.zero_amount,
                earlier_rule: ran.earlier_rule,
            },
        }
    }
}

/// What taking back a rule's work did, as its results page's address holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TakenBack {
    changed: u32,
    categorized: u32,
    assigned: u32,
    reviewed: u32,
}

impl From<RuleUndone> for TakenBack {
    fn from(done: RuleUndone) -> Self {
        Self {
            changed: done.changed,
            categorized: done.categorized,
            assigned: done.assigned,
            reviewed: done.reviewed,
        }
    }
}

impl From<TakenBack> for RuleUndone {
    fn from(back: TakenBack) -> Self {
        Self {
            changed: back.changed,
            categorized: back.categorized,
            assigned: back.assigned,
            reviewed: back.reviewed,
        }
    }
}

/// `/rules/{id}/{page}?{counts}`.
fn results(id: RuleId, page: &str, counts: &impl Serialize) -> Result<Redirect, PageError> {
    let query = serde_urlencoded::to_string(counts)
        .map_err(|_| PageError::invalid("these counts can't be shown"))?;
    Ok(Redirect::to(&format!("/rules/{id}/{page}?{query}")))
}

/// The counts in a results page's address, every one a whole number and nothing else.
fn counts<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, PageError> {
    let Query(counts) = query.map_err(|_| PageError::invalid("not counts this page shows"))?;
    Ok(counts)
}

/// The page that says what running or taking back did, with the way back to the rule.
async fn outcome(
    state: &AppState,
    session: &PageSession,
    id: RuleId,
    heading: &str,
    sentences: &[String],
    left: &[String],
) -> Response {
    let viewer = viewer(state, session).await;
    let frame = Frame {
        look: session.look(state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Rules),
        narrow: false,
    };
    let content = html! {
        h1 { (heading) }
        section.card id="outcome" role="status" {
            @for sentence in sentences { p { (sentence) } }
            @if !left.is_empty() {
                h3 { "What it matches and left alone" }
                ul.reasons {
                    @for sentence in left { li { (sentence) } }
                }
            }
            p { a href={ "/rules/" (id) } { "Back to the rule" } }
        }
    };
    page(&frame, heading, &content).into_response()
}

/// Runs the rule by hand. The household's right to write is checked before the form is read;
/// the rule's account is the ledger's to check.
pub(super) async fn apply(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ButtonForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let id = rule_id(&id)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let done = state.ledger.apply_rule(&actor, id).await.map_err(refused)?;
    results(id, "ran", &Ran::from(done))
}

/// What running the rule did, named with its category and member as they are now: what it
/// filled was those.
pub(super) async fn ran(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    query: Result<Query<Ran>, QueryRejection>,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let id = rule_id(&id)?;
    let done = RuleApplied::from(counts(query)?);
    let rule = state.ledger.get_rule(&actor, id).await.map_err(refused)?;
    let names = Names::load(&state, &actor).await?;
    let sentences = applied_sentences(done, rule.category, rule.assignee, &names);
    let left = left_alone_sentences(done.left_alone);
    Ok(outcome(&state, &session, id, "Rule run", &sentences, &left).await)
}

/// Takes back what the rule filled.
pub(super) async fn undo(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    form: Result<PageForm<ButtonForm>, PageError>,
) -> Result<Redirect, PageError> {
    authorize(&session.actor(), Action::WriteHousehold).map_err(AuthError::from)?;
    let id = rule_id(&id)?;
    let PageForm(form) = form?;
    let actor = session.verify(&state, &form.csrf)?;
    let done = state.ledger.undo_rule(&actor, id).await.map_err(refused)?;
    results(id, "taken-back", &TakenBack::from(done))
}

/// What taking back did.
pub(super) async fn taken_back(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    query: Result<Query<TakenBack>, QueryRejection>,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let id = rule_id(&id)?;
    let done = RuleUndone::from(counts(query)?);
    // Only a rule the viewer may see has a page.
    state.ledger.get_rule(&actor, id).await.map_err(refused)?;
    let sentences = undone_sentences(done);
    Ok(outcome(&state, &session, id, "Taken back", &sentences, &[]).await)
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;
    use axum::http::Uri;

    use super::{Ran, TakenBack};

    fn read<T: serde::de::DeserializeOwned>(query: &str) -> Option<T> {
        let uri: Uri = format!("/x?{query}").parse().ok()?;
        Query::<T>::try_from_uri(&uri).ok().map(|Query(t)| t)
    }

    #[test]
    fn counts_go_to_the_address_and_come_back_the_same() {
        let back = TakenBack {
            changed: 3,
            categorized: 2,
            assigned: 1,
            reviewed: 4,
        };
        let query = serde_urlencoded::to_string(back).unwrap();
        assert_eq!(query, "changed=3&categorized=2&assigned=1&reviewed=4");
        assert_eq!(read::<TakenBack>(&query), Some(back));
    }

    #[test]
    fn anything_but_whole_counts_is_refused() {
        let good = "changed=3&categorized=2&assigned=1&reviewed=0";
        for query in [
            "changed=-1&categorized=2&assigned=1&reviewed=0",
            "changed=1.5&categorized=2&assigned=1&reviewed=0",
            "changed=x&categorized=2&assigned=1&reviewed=0",
            "changed=3&categorized=2&assigned=1",
            "changed=3&categorized=2&assigned=1&reviewed=0&note=hi",
            "changed=99999999999&categorized=2&assigned=1&reviewed=0",
        ] {
            assert_eq!(read::<TakenBack>(query), None, "refused");
        }
        assert!(read::<TakenBack>(good).is_some());
        assert_eq!(
            read::<Ran>(good),
            None,
            "a take-back's counts aren't a run's"
        );
    }
}
