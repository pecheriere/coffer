//! One transaction's page: what `GET /api/v1/transactions/{id}` answers, in words, with the
//! forms that review it, file or split it, edit its payee, notes and assignee, and delete one
//! entered by hand.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use coffer_domain::transactions::{ReviewStatus, TransactionOrigin};
use maud::{Markup, html};

use super::splits::{category_form, splits_form};
use super::{LIST, Names, chips, detail_path, refused, title, transaction_id};
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::amounts::{day, money};
use crate::http::pages::layout::{Frame, Nav, csrf_field, page as frame_page};
use crate::http::pages::{PageError, PageSession};
use crate::ledger::transactions::TransactionView;

pub(super) async fn page(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let id = transaction_id(&id)?;
    let actor = session.actor();
    let transaction = state
        .ledger
        .get_transaction(&actor, id)
        .await
        .map_err(refused)?;
    let names = Names::read(&state, &actor).await?;
    let viewer = viewer(&state, &session).await;
    let frame = Frame {
        look: session.look(&state).await,
        viewer: Some(&viewer),
        current: Some(Nav::Transactions),
        narrow: false,
    };
    let content = content(&transaction, &names, &viewer.csrf);
    Ok(frame_page(&frame, title(&transaction), &content).into_response())
}

fn content(transaction: &TransactionView, names: &Names, csrf: &str) -> Markup {
    html! {
        p { a href=(LIST) { "All transactions" } }
        h1 { (title(transaction)) (chips(transaction)) }
        (summary(transaction, names))
        (review_card(transaction, names, csrf))
        section.card id="category" {
            h2 { "Filed as" }
            @if transaction.splits.len() > 1 {
                (parts(transaction, names))
            }
            @if let Some(by) = &transaction.categorized_by {
                p.fine { "Last filed by " (names.by(by)) "." }
            }
            (category_form(transaction, names, csrf))
            (splits_form(transaction, names, csrf))
        }
        (details_card(transaction, names, csrf))
        @if transaction.origin == TransactionOrigin::Manual {
            (delete_card(transaction, csrf))
        }
    }
}

fn summary(transaction: &TransactionView, names: &Names) -> Markup {
    html! {
        section.card id="summary" {
            table.sums {
                tbody {
                    tr { th scope="row" { "Amount" } td { (money(transaction.amount)) } }
                    tr { th scope="row" { "Posted" } td { (day(transaction.posted_on)) } }
                    @if let Some(made) = transaction.transacted_on {
                        tr { th scope="row" { "Made" } td { (day(made)) } }
                    }
                    tr { th scope="row" { "Account" } td { (names.account(transaction.account)) } }
                    tr { th scope="row" { "Description" } td { (transaction.description) } }
                    @if let Some(memo) = &transaction.memo {
                        tr { th scope="row" { "Memo" } td { (memo) } }
                    }
                    tr {
                        th scope="row" { "From" }
                        td {
                            @match transaction.origin {
                                TransactionOrigin::Manual => "Entered by hand",
                                TransactionOrigin::Imported => "Imported from the bank",
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The parts of a split transaction.
fn parts(transaction: &TransactionView, names: &Names) -> Markup {
    html! {
        table.sums {
            tbody {
                @for split in &transaction.splits {
                    tr {
                        th scope="row" {
                            (names.category(split.category))
                            @if let Some(goal) = split.goal {
                                ", towards " (names.goal(goal))
                            }
                        }
                        td { (money(split.amount)) }
                    }
                }
            }
        }
    }
}

fn review_card(transaction: &TransactionView, names: &Names, csrf: &str) -> Markup {
    let reviewed = transaction.review == ReviewStatus::Reviewed;
    html! {
        section.card id="review" {
            h2 { "Review" }
            @if let (true, Some(by)) = (reviewed, &transaction.reviewed_by) {
                p { "Reviewed by " (names.by(by)) "." }
            } @else {
                p { "Nobody has reviewed it yet." }
            }
            form method="post" action={ (detail_path(transaction.id)) "/review" } {
                (csrf_field(csrf))
                @if reviewed {
                    p.fine { "Putting it back to review forgets who reviewed it. Nothing else changes." }
                    button.btn type="submit" name="review" value="unreviewed" { "Put it back to review" }
                } @else {
                    p.fine { "Marking it reviewed records that you did. Nothing else changes." }
                    button.btn.btn-primary type="submit" name="review" value="reviewed" { "Mark it reviewed" }
                }
            }
        }
    }
}

fn details_card(transaction: &TransactionView, names: &Names, csrf: &str) -> Markup {
    html! {
        section.card id="details" {
            h2 { "Payee, notes and who it's for" }
            form method="post" action={ (detail_path(transaction.id)) "/details" } {
                (csrf_field(csrf))
                label.field {
                    "Payee"
                    input.input type="text" name="payee" maxlength="200" autocomplete="off"
                        value=[transaction.payee.as_deref()];
                    span.fine {
                        "Once you change it, a sync no longer does. Leave it empty to go back to "
                        "the payee the bank gave."
                    }
                }
                label.field {
                    "Notes"
                    textarea.input name="notes" maxlength="2000" rows="3" {
                        // A parser drops one line break straight after the tag: this one, so
                        // notes that start with a line break keep it.
                        "\n" (transaction.notes.as_deref().unwrap_or_default())
                    }
                }
                label.field {
                    "Assigned to"
                    select.input name="assignee" {
                        option value="" selected[transaction.assignee.is_none()] { "Nobody" }
                        @for member in &names.members {
                            @if member.active || transaction.assignee == Some(member.id) {
                                option value=(member.id) selected[transaction.assignee == Some(member.id)] {
                                    (member.name)
                                }
                            }
                        }
                    }
                }
                @if let Some(by) = &transaction.assigned_by {
                    p.fine { "Last assigned by " (names.by(by)) "." }
                }
                button.btn.btn-primary type="submit" { "Save" }
            }
        }
    }
}

fn delete_card(transaction: &TransactionView, csrf: &str) -> Markup {
    html! {
        section.card id="delete" {
            h2 { "Delete" }
            p.fine {
                "It was entered by hand, so it can be deleted, with its parts. "
                "The audit log keeps what it was. One in a transfer link stays until the link is removed."
            }
            form method="post" action={ (detail_path(transaction.id)) "/delete" } {
                (csrf_field(csrf))
                button.btn type="submit" { "Delete this transaction" }
            }
        }
    }
}
