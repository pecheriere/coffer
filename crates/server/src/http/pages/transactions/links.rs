//! A transaction's transfer link (spec §8.12): the link it is one leg of, read as
//! `GET /api/v1/transfer-links/{id}` answers it, and the form that removes it; or the
//! transactions that might be its other leg, as `listTransferCandidates` suggests them, each
//! with a button that links it, and a form naming any other one, of another amount or currency.

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::ids::TransactionId;
use coffer_domain::transactions::TransactionFlag;
use coffer_domain::transfer_rate::{RATE_TOLERANCE_PERCENT, RateCheck};
use maud::{Markup, html};
use serde::Deserialize;

use super::{Names, detail_path, refused, title, transaction_id};
use crate::http::AppState;
use crate::http::pages::amounts::{day, magnitude, money};
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::LedgerError;
use crate::ledger::transactions::TransactionView;
use crate::ledger::transfer_links::{
    INFLOW_SIGN, OUTFLOW_SIGN, TransferCandidates, TransferLegView, TransferLinkView,
};

/// What the page knows of a transaction's link: the link, or what might be its other leg.
pub(super) enum LinkState {
    Linked(Box<TransferLinkView>),
    Unlinked(TransferCandidates),
}

impl LinkState {
    /// The link `transaction` is one leg of, or its candidates when it has none, or none any
    /// more: a link removed since the transaction was read shows it unlinked.
    pub(super) async fn read(
        state: &AppState,
        session: &PageSession,
        transaction: &TransactionView,
    ) -> Result<Self, PageError> {
        let actor = session.actor();
        if let Some(link) = transaction.transfer_link {
            match state.ledger.get_transfer_link(&actor, link.id).await {
                Ok(link) => return Ok(Self::Linked(Box::new(link))),
                Err(LedgerError::NotFound) => {}
                Err(err) => return Err(refused(err)),
            }
        }
        state
            .ledger
            .transfer_candidates(&actor, transaction.id)
            .await
            .map(Self::Unlinked)
            .map_err(refused)
    }
}

const ONE_MOVEMENT: &str = "Both accounts are cash the forecast counts, so what moved is one \
    movement: neither spending nor income, whatever each leg is filed as.";
const RECORDED_ONLY: &str = "One account isn't cash the forecast counts, so the link records the \
    movement and changes no number: each leg counts as what it is filed as.";
const LINK_FINE: &str = "Linking says the two are one movement of money between your accounts. \
    Each keeps its category, and removing the link puts them back as they were.";
const NO_CANDIDATES: &str = "No transaction you can see looks like its other leg.";
const SPLIT_LEG: &str = "Split in more than one part, it can't be one leg of a transfer: part of \
    it may be spending. File it in one category to link it.";
const OTHER_TAKEN_BACK: &str = "The other leg was taken back by the bank. The link stays until \
    you remove it.";
const REVISED: &str = "The bank changed a leg since they were linked, and the two no longer go \
    one out and one in. Check that they are still one movement.";
/// The service's refusal of two legs that go the same way, in the page's words: it names the
/// API's fields, which the page doesn't show.
const SAME_WAY: &str = "Link money out with money in: these two go the same way.";

pub(super) fn card(
    transaction: &TransactionView,
    link: &LinkState,
    names: &Names,
    csrf: &str,
) -> Markup {
    html! {
        section.card id="transfer" {
            h2 { "Transfer" }
            @match link {
                LinkState::Linked(link) => (linked(transaction, link, names, csrf)),
                LinkState::Unlinked(candidates) => (unlinked(transaction, candidates, names, csrf)),
            }
        }
    }
}

/// A leg's day, and whether it is still pending.
fn when(leg: &TransferLegView) -> Markup {
    html! {
        (day(leg.posted_on))
        @if leg.pending { ", pending" }
    }
}

/// A link, with the form that removes it for an actor who can change both accounts.
fn linked(
    transaction: &TransactionView,
    link: &TransferLinkView,
    names: &Names,
    csrf: &str,
) -> Markup {
    let other = if link.outflow.transaction == transaction.id {
        &link.inflow
    } else {
        &link.outflow
    };
    let (outflow, inflow) = (&link.outflow, &link.inflow);
    let out_and_in = outflow.amount.minor_units < 0 && inflow.amount.minor_units > 0;
    let writes = names.may_write(outflow.account) && names.may_write(inflow.account);
    html! {
        @if out_and_in {
            p {
                (magnitude(outflow.amount)) " left " (names.account(outflow.account))
                " on " (when(outflow)) ", and " (magnitude(inflow.amount))
                " reached " (names.account(inflow.account)) " on " (when(inflow)) "."
            }
        } @else {
            p { (REVISED) }
            p {
                (money(outflow.amount)) " in " (names.account(outflow.account)) " on "
                (when(outflow)) ", and " (money(inflow.amount)) " in "
                (names.account(inflow.account)) " on " (when(inflow)) "."
            }
        }
        p { a href=(detail_path(other.transaction)) { "The other leg" } }
        @if other.flags.contains(&TransactionFlag::SourceRemoved) {
            p { (OTHER_TAKEN_BACK) }
        }
        @if let Some(difference) = link
            .difference
            .filter(|difference| out_and_in && difference.minor_units != 0)
        {
            p {
                @if difference.minor_units < 0 {
                    (magnitude(difference)) " less arrived than left, such as for a fee. "
                } @else {
                    (magnitude(difference)) " more arrived than left. "
                }
                "That difference counts as what its leg is filed as."
            }
        }
        @if let Some(check) = &link.rate_check {
            p { (rate_words(check)) }
        }
        p.fine { (if link.one_movement { ONE_MOVEMENT } else { RECORDED_ONLY }) }
        @if writes {
            form method="post" action={ (detail_path(transaction.id)) "/unlink" } {
                (csrf_field(csrf))
                p.fine {
                    "Removing the link leaves both transactions as they are, and each counts again "
                    "as what it is filed as. The audit log keeps what was joined."
                }
                button.btn type="submit" { "Remove the link" }
            }
        }
    }
}

fn rate_words(check: &RateCheck) -> String {
    match check {
        RateCheck::Agrees { rate_on } => format!(
            "The two currencies agree at the exchange rate of {}.",
            day(*rate_on)
        ),
        RateCheck::Mismatch(mismatch) => format!(
            "At the exchange rate of {}{}, what left comes to {}, more than \
             {RATE_TOLERANCE_PERCENT}% away from what arrived. Check that these are the two legs \
             of one movement. No amount changes.",
            day(mismatch.rate_on),
            if mismatch.manual_rate {
                ", entered by hand"
            } else {
                ""
            },
            money(mismatch.expected_inflow),
        ),
        RateCheck::NotChecked => "No usable exchange rate is held for that day, so the two \
                                  currencies aren't compared."
            .to_owned(),
    }
}

/// What might be the other leg, each with the button that links it when the actor can change
/// both accounts, and the form that names any other one when it can change this one.
fn unlinked(
    transaction: &TransactionView,
    candidates: &TransferCandidates,
    names: &Names,
    csrf: &str,
) -> Markup {
    if transaction.splits.len() > 1 {
        return html! { p { (SPLIT_LEG) } };
    }
    let writes = names.may_write(transaction.account);
    let action = format!("{}/link", detail_path(transaction.id));
    html! {
        @if writes {
            p.fine { (LINK_FINE) }
        }
        @if candidates.items.is_empty() {
            p { (NO_CANDIDATES) }
        } @else {
            p { "These might be its other leg: the same amount the other way, within five days." }
            table.sums {
                tbody {
                    @for candidate in &candidates.items {
                        tr {
                            th scope="row" {
                                a href=(detail_path(candidate.id)) { (title(candidate)) }
                                ", " (names.account(candidate.account)) ", " (day(candidate.posted_on))
                            }
                            td { (money(candidate.amount)) }
                            @if writes && names.may_write(candidate.account) {
                                td {
                                    form method="post" action=(action) {
                                        (csrf_field(csrf))
                                        input type="hidden" name="other" value=(candidate.id);
                                        button.btn type="submit" aria-label={ "Link with " (title(candidate)) } {
                                            "Link"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if writes {
            details.item-detail {
                summary { "Link it with another transaction" }
                form method="post" action=(action) {
                    (csrf_field(csrf))
                    p.fine {
                        "For a leg of another amount, such as after a fee, or in another currency, "
                        "which is checked against the exchange rate once linked. Paste the other "
                        "transaction's address, from its page."
                    }
                    label.field {
                        "The other transaction's address"
                        input.input type="text" name="other" required autocomplete="off";
                    }
                    button.btn type="submit" { "Link" }
                }
            }
        }
    }
}

/// A transaction's id, or the address of its page.
fn other_leg(text: &str) -> Result<TransactionId, PageError> {
    let text = text.trim();
    let address = text.split(['?', '#']).next().unwrap_or(text);
    let address = address.trim_end_matches('/');
    let id = address.rsplit('/').next().unwrap_or(address);
    id.parse()
        .map_err(|_| PageError::invalid("not the address of a transaction's page"))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    other: String,
}

/// Links the transaction with `other`, the one with money out as the outflow, as
/// `POST /api/v1/transfer-links` does.
pub(super) async fn link(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<LinkForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    let other = other_leg(&form.other)?;
    let current = state
        .ledger
        .get_transaction(&actor, id)
        .await
        .map_err(refused)?;
    let (outflow, inflow) = if current.amount.minor_units < 0 {
        (id, other)
    } else {
        (other, id)
    };
    state
        .ledger
        .create_transfer_link(&actor, outflow, inflow, None)
        .await
        .map_err(|err| match err {
            LedgerError::Invalid(words) if words == OUTFLOW_SIGN || words == INFLOW_SIGN => {
                PageError::invalid(SAME_WAY)
            }
            err => refused(err),
        })?;
    Ok(Redirect::to(&detail_path(id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnlinkForm {
    #[serde(default)]
    csrf: String,
}

/// Removes the link the transaction of the path is one leg of, as
/// `DELETE /api/v1/transfer-links/{id}` does: the form names no link, so it can't end another.
pub(super) async fn unlink(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<UnlinkForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    let current = state
        .ledger
        .get_transaction(&actor, id)
        .await
        .map_err(refused)?;
    let Some(link) = current.transfer_link else {
        return Err(PageError::not_found());
    };
    state
        .ledger
        .remove_transfer_link(&actor, link.id)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)))
}

#[cfg(test)]
mod tests;
