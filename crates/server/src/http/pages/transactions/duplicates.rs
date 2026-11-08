//! Possible duplicates (spec §6.2, §8.4): the transactions one may duplicate, each with the
//! forms that merge the two either way or say it isn't a duplicate, and the records a merged
//! one holds, each with the form that moves it back into a transaction of its own. They do what
//! `mergeTransactions`, `dismissDuplicate` and `unmergeTransaction` do.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Redirect;
use coffer_domain::ids::TransactionId;
use coffer_domain::transactions::TransactionFlag;
use maud::{Markup, html};
use serde::Deserialize;

use super::{Names, detail_path, refused, title, transaction_id};
use crate::http::AppState;
use crate::http::pages::amounts::{day, money};
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::http::problem::{Problem, ProblemCode};
use crate::ledger::LedgerError;
use crate::ledger::transactions::TransactionView;

const MERGE_FINE: &str = "Merging moves the other's bank records into the one kept and deletes the \
    other. The one kept keeps its own category, parts, review, notes, assignee and payee, so keep \
    the one that holds them.";
const DISCARD: &str = "If the one deleted holds any of those, or a transfer link, discard them.";
/// The service's conflict when the box wasn't ticked, in the page's words: it names the API's
/// field, which the page doesn't show.
const MERGE_HOLDS: &str = "The one to delete holds a category, parts, a review, notes, an \
    assignee, a payee of its own or a transfer link. To merge anyway, tick the box to discard them.";
const CANDIDATES_GONE: &str = "The ones it may duplicate are gone.";
const UNMERGE_FINE: &str = "Moving a record out makes it a transaction of its own, with nothing \
    filed or reviewed. This one keeps what it holds. Later syncs keep them apart.";

/// The transactions `transaction` may duplicate, as the actor sees them. One gone since, merged
/// or deleted, is left out.
pub(super) async fn candidates(
    state: &AppState,
    session: &PageSession,
    transaction: &TransactionView,
) -> Result<Vec<TransactionView>, PageError> {
    if !transaction
        .flags
        .contains(&TransactionFlag::PossibleDuplicate)
    {
        return Ok(Vec::new());
    }
    let actor = session.actor();
    let mut found = Vec::new();
    for id in &transaction.duplicate_candidates {
        match state.ledger.get_transaction(&actor, *id).await {
            Ok(candidate) => found.push(candidate),
            Err(LedgerError::NotFound) => {}
            Err(err) => return Err(refused(err)),
        }
    }
    Ok(found)
}

/// The possible duplicates and the merged records, each with the forms the actor may use;
/// nothing when there are neither.
pub(super) fn card(
    transaction: &TransactionView,
    candidates: &[TransactionView],
    names: &Names,
    csrf: &str,
) -> Markup {
    let flagged = transaction
        .flags
        .contains(&TransactionFlag::PossibleDuplicate);
    let merged = transaction.sources.len() > 1;
    if !flagged && !merged {
        return html! {};
    }
    let writes = names.may_write(transaction.account);
    html! {
        section.card id="duplicates" {
            h2 { "Duplicates" }
            @if flagged {
                @if candidates.is_empty() {
                    p { (CANDIDATES_GONE) }
                } @else {
                    p { "This may be the same purchase as:" }
                    @for candidate in candidates {
                        (candidate_item(transaction, candidate, names, csrf))
                    }
                }
                @if writes {
                    form method="post" action={ (detail_path(transaction.id)) "/dismiss-duplicate" } {
                        (csrf_field(csrf))
                        p.fine {
                            "Saying it isn't a duplicate clears the mark and forgets these, here and "
                            "on their side. Nothing else changes."
                        }
                        button.btn type="submit" { "It isn't a duplicate" }
                    }
                }
            }
            @if merged {
                h3 { "Bank records merged into it" }
                @if writes {
                    p.fine { (UNMERGE_FINE) }
                }
                table.sums {
                    tbody {
                        @for source in &transaction.sources {
                            tr {
                                th scope="row" {
                                    (day(source.posted_on))
                                    @if source.pending { ", pending" }
                                }
                                td { (source.amount_raw) " as the bank wrote it" }
                                @if writes {
                                    td {
                                        form method="post" action={ (detail_path(transaction.id)) "/unmerge" } {
                                            (csrf_field(csrf))
                                            input type="hidden" name="source" value=(source.id);
                                            button.btn type="submit" { "Move it out" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A possible duplicate, with the forms that merge the two either way for an actor who can change
/// both accounts.
fn candidate_item(
    transaction: &TransactionView,
    candidate: &TransactionView,
    names: &Names,
    csrf: &str,
) -> Markup {
    html! {
        div.item {
            div.item-main {
                p.item-title { a href=(detail_path(candidate.id)) { (title(candidate)) } }
                p.item-meta {
                    (money(candidate.amount)) ", " (day(candidate.posted_on)) ", "
                    (names.account(candidate.account))
                    @if candidate.pending { ", pending" }
                }
            }
            @if names.may_write(transaction.account) && names.may_write(candidate.account) {
                details.item-detail {
                    summary { "Merge" }
                    p.fine { (MERGE_FINE) }
                    (merge_form(transaction.id, candidate.id, "Keep this one, delete that one", csrf))
                    (merge_form(candidate.id, transaction.id, "Keep that one, delete this one", csrf))
                }
            }
        }
    }
}

/// The form that merges `duplicate` into `target`.
fn merge_form(target: TransactionId, duplicate: TransactionId, label: &str, csrf: &str) -> Markup {
    html! {
        form method="post" action={ (detail_path(target)) "/merge" } {
            (csrf_field(csrf))
            input type="hidden" name="duplicate" value=(duplicate);
            label.choice {
                input type="checkbox" name="discard" value="yes";
                (DISCARD)
            }
            button.btn type="submit" { (label) }
        }
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MergeForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    duplicate: String,
    #[serde(default)]
    discard: String,
}

/// Merges the form's duplicate into the transaction of the path, then shows the one kept.
pub(super) async fn merge(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<MergeForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let target = transaction_id(&id)?;
    let duplicate: TransactionId = form
        .duplicate
        .parse()
        .map_err(|_| PageError::invalid("the form could not be read"))?;
    let discard = match form.discard.as_str() {
        "" => false,
        "yes" => true,
        _ => return Err(PageError::invalid("not an answer the form offers")),
    };
    match state.ledger.merge(&actor, target, duplicate, discard).await {
        Ok(_) => Ok(Redirect::to(&detail_path(target))),
        Err(LedgerError::Conflict(ProblemCode::MergeConflict, _)) if !discard => Err(PageError(
            Problem::new(StatusCode::CONFLICT, ProblemCode::MergeConflict, "Conflict")
                .with_detail(MERGE_HOLDS.to_owned()),
        )),
        Err(err) => Err(refused(err)),
    }
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnmergeForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    source: String,
}

/// Moves the form's record into a transaction of its own, then shows that one.
pub(super) async fn unmerge(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<UnmergeForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    let source = form
        .source
        .parse()
        .map_err(|_| PageError::invalid("the form could not be read"))?;
    let created = state
        .ledger
        .unmerge(&actor, id, source, None)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(created.id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DismissForm {
    #[serde(default)]
    csrf: String,
}

/// Says the transaction isn't a duplicate of its candidates, as `dismissDuplicate` does.
pub(super) async fn dismiss(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<DismissForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    state
        .ledger
        .dismiss_duplicate(&actor, id)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)))
}

#[cfg(test)]
mod tests;
