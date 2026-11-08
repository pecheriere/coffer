//! The list: one page of what `GET /api/v1/transactions` answers for the filters chosen, newest
//! first, with the form that marks the ticked ones reviewed and the one that enters a transaction
//! by hand. The review queue is the list of what is still to review.

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use coffer_domain::transactions::ReviewStatus;
use maud::{Markup, html};

use super::filters::{CategoryChoice, FLAGS, Filters};
use super::{LIST, Names, chips, detail_path, filed_as, flag_words, manual, refused, title};
use crate::http::AppState;
use crate::http::pages::account::viewer;
use crate::http::pages::amounts::{day, money};
use crate::http::pages::layout::{Frame, Nav, csrf_field, empty_state, page as frame_page};
use crate::http::pages::{PageError, PageSession};
use crate::ledger::transactions::{TransactionPage, TransactionView};
use crate::themes::Look;

/// The review queue: what nobody has reviewed yet.
const QUEUE: &str = "/transactions?review=unreviewed";

pub(super) async fn page(
    State(state): State<AppState>,
    session: PageSession,
    RawQuery(query): RawQuery,
) -> Result<Response, PageError> {
    let (filters, after) = Filters::parse(query.as_deref())?;
    let actor = session.actor();
    let found = state
        .ledger
        .list_transactions(&actor, filters.input(after.clone())?)
        .await
        .map_err(refused)?;
    let names = Names::read(&state, &actor).await?;
    let viewer = viewer(&state, &session).await;
    let look = session.look(&state).await;
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::Activity),
        narrow: false,
    };
    let view = ListView {
        filters: &filters,
        first_page: after.is_none(),
        found: &found,
        names: &names,
        csrf: &viewer.csrf,
        look,
    };
    Ok(frame_page(&frame, "Transactions", &content(&view)).into_response())
}

struct ListView<'a> {
    filters: &'a Filters,
    first_page: bool,
    found: &'a TransactionPage,
    names: &'a Names,
    csrf: &'a str,
    look: Look,
}

fn content(view: &ListView<'_>) -> Markup {
    let queue = view.filters.review == Some(ReviewStatus::Unreviewed);
    html! {
        h1 { (if queue { "To review" } else { "Transactions" }) }
        p.ranges {
            a href=(LIST) aria-current=[(view.filters == &Filters::default()).then_some("page")] { "All" }
            " "
            a href=(QUEUE) aria-current=[queue.then_some("page")] { "To review" }
        }
        (filter_form(view.filters, view.names))
        section.card id="transactions" {
            @if view.found.items.is_empty() {
                (empty_state(view.look, if queue {
                    "Nothing left to review."
                } else {
                    "No transactions match."
                }))
            } @else {
                (table(view))
            }
            @if let Some(next) = &view.found.next {
                p { a href=(view.filters.path_after(next)) { "Older transactions" } }
            }
            @if !view.first_page {
                p.fine { a href=(view.filters.path()) { "Back to the newest" } }
            }
        }
        (manual::form(view.names, view.csrf))
    }
}

/// The filters, as a form that asks for the list again. Its fields are those
/// [`Filters::parse`] reads.
fn filter_form(filters: &Filters, names: &Names) -> Markup {
    let from = filters.from.map(|on| on.to_string());
    let until = filters.until.map(|on| on.to_string());
    html! {
        details.card.values open[*filters != Filters::default()] {
            summary { "Filter" }
            form method="get" action=(LIST) {
                label.field {
                    "Account"
                    select.input name="account" {
                        option value="" { "Every account" }
                        @for account in names.transaction_accounts() {
                            option value=(account.id) selected[filters.account == Some(account.id)] {
                                (account.name)
                            }
                        }
                    }
                }
                label.field {
                    "From"
                    input.input type="date" name="from" value=[from];
                }
                label.field {
                    "Until, included"
                    input.input type="date" name="until" value=[until];
                }
                label.field {
                    "Review"
                    select.input name="review" {
                        option value="" { "Reviewed or not" }
                        option value="unreviewed" selected[filters.review == Some(ReviewStatus::Unreviewed)] {
                            "To review"
                        }
                        option value="reviewed" selected[filters.review == Some(ReviewStatus::Reviewed)] {
                            "Reviewed"
                        }
                    }
                }
                label.field {
                    "Category"
                    select.input name="category" {
                        option value="" { "Any category" }
                        option value="none" selected[filters.category == CategoryChoice::Uncategorized] {
                            "Uncategorized"
                        }
                        @for category in &names.categories {
                            option value=(category.id)
                                selected[filters.category == CategoryChoice::One(category.id)] {
                                (category.label)
                            }
                        }
                    }
                }
                label.field {
                    "Marked"
                    select.input name="flag" {
                        option value="" { "With or without a mark" }
                        @for flag in FLAGS {
                            option value=(flag.as_str()) selected[filters.flag == Some(flag)] {
                                (flag_words(flag))
                            }
                        }
                    }
                }
                label.field {
                    "Words in the description, payee or notes"
                    input.input type="search" name="q" maxlength="100" value=[filters.text.as_deref()];
                }
                button.btn.btn-primary type="submit" { "Show" }
                " "
                a href=(LIST) { "Clear the filters" }
            }
        }
    }
}

/// One page of transactions inside the form that marks the ticked ones reviewed. Below the
/// phone width each row stands alone, so `data-label` repeats its column's heading. Only a
/// transaction the actor may change can be ticked, and with none, the form has no buttons.
fn table(view: &ListView<'_>) -> Markup {
    let columns = ["Day", "Account", "Amount", "Filed as"];
    let any_writable = view
        .found
        .items
        .iter()
        .any(|transaction| view.names.may_write(transaction.account));
    html! {
        form method="post" action="/transactions/batch" {
            (csrf_field(view.csrf))
            input type="hidden" name="back" value=(view.filters.query());
            table.months role="table" {
                thead role="rowgroup" {
                    tr role="row" {
                        th scope="col" role="columnheader" { "Transaction" }
                        @for column in columns {
                            th scope="col" role="columnheader" { (column) }
                        }
                        th scope="col" role="columnheader" { "Tick" }
                    }
                }
                tbody role="rowgroup" {
                    @for transaction in &view.found.items {
                        (row(transaction, view.names))
                    }
                }
            }
            @if any_writable {
                p.fine {
                    "Marking them reviewed records that you did, and changes nothing else about them. "
                    "All the ticked ones change together, or none do."
                }
                button.btn type="submit" name="review" value="reviewed" { "Mark the ticked ones reviewed" }
                " "
                button.btn type="submit" name="review" value="unreviewed" { "Mark the ticked ones to review" }
            }
        }
    }
}

fn row(transaction: &TransactionView, names: &Names) -> Markup {
    let name = title(transaction);
    html! {
        tr role="row" {
            th scope="row" role="rowheader" {
                a href=(detail_path(transaction.id)) { (name) }
                (chips(transaction))
            }
            td role="cell" data-label="Day" { (day(transaction.posted_on)) }
            td role="cell" data-label="Account" { (names.account(transaction.account)) }
            td role="cell" data-label="Amount" { (money(transaction.amount)) }
            td role="cell" data-label="Filed as" { (filed_as(transaction, names)) }
            td role="cell" data-label="Tick" {
                @if names.may_write(transaction.account) {
                    input type="checkbox" name={ "pick_" (transaction.id) } value="yes"
                        aria-label={ "Tick " (name) };
                }
            }
        }
    }
}
