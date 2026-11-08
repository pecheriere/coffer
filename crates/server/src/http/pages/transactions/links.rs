//! A transaction's transfer link (spec §8.12): the link it is one leg of, read as
//! `GET /api/v1/transfer-links/{id}` answers it, and the form that removes it; or the
//! transactions that might be its other leg, as `listTransferCandidates` suggests them, each
//! with a button that links it, and a form naming any other one, of another amount or currency.

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::ids::{TransactionId, TransferLinkId};
use coffer_domain::transfer_rate::RateCheck;
use maud::{Markup, html};
use serde::Deserialize;

use super::{Names, detail_path, refused, title, transaction_id};
use crate::http::AppState;
use crate::http::pages::amounts::{day, magnitude, money};
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::transactions::TransactionView;
use crate::ledger::transfer_links::{TransferCandidates, TransferLinkView};

/// What the page knows of a transaction's link: the link, or what might be its other leg.
pub(super) enum LinkState {
    Linked(Box<TransferLinkView>),
    Unlinked(TransferCandidates),
}

impl LinkState {
    pub(super) async fn read(
        state: &AppState,
        session: &PageSession,
        transaction: &TransactionView,
    ) -> Result<Self, PageError> {
        let actor = session.actor();
        match transaction.transfer_link {
            Some(link) => state
                .ledger
                .get_transfer_link(&actor, link.id)
                .await
                .map(|link| Self::Linked(Box::new(link)))
                .map_err(refused),
            None => state
                .ledger
                .transfer_candidates(&actor, transaction.id)
                .await
                .map(Self::Unlinked)
                .map_err(refused),
        }
    }
}

const ONE_MOVEMENT: &str = "Both accounts are cash the forecast counts, so what moved is one \
    movement: neither spending nor income, whatever each leg is filed as.";
const RECORDED_ONLY: &str = "One account isn't cash the forecast counts, so the link records the \
    movement and changes no number: each leg counts as what it is filed as.";
const LINK_FINE: &str = "Linking says the two are one movement of money between your accounts. \
    Each keeps its category, and removing the link puts them back as they were.";

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
    html! {
        p {
            (magnitude(link.outflow.amount)) " left " (names.account(link.outflow.account))
            " on " (day(link.outflow.posted_on)) ", and " (magnitude(link.inflow.amount))
            " reached " (names.account(link.inflow.account)) " on " (day(link.inflow.posted_on)) "."
        }
        p { a href=(detail_path(other.transaction)) { "The other leg" } }
        @if let Some(difference) = link.difference.filter(|difference| difference.minor_units != 0) {
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
        form method="post" action={ (detail_path(transaction.id)) "/unlink" } {
            (csrf_field(csrf))
            input type="hidden" name="link" value=(link.id);
            p.fine {
                "Removing the link leaves both transactions as they are, and each counts again as "
                "what it is filed as. The audit log keeps what was joined."
            }
            button.btn type="submit" { "Remove the link" }
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
            "At the exchange rate of {}{}, what left comes to {}, more than 5% away from what \
             arrived. Check that these are the two legs of one movement. No amount changes.",
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

fn unlinked(
    transaction: &TransactionView,
    candidates: &TransferCandidates,
    names: &Names,
    csrf: &str,
) -> Markup {
    let action = format!("{}/link", detail_path(transaction.id));
    html! {
        p.fine { (LINK_FINE) }
        @if candidates.items.is_empty() {
            p { "coffer sees nothing that looks like its other leg." }
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

/// A transaction's id, or the address of its page.
fn other_leg(text: &str) -> Result<TransactionId, PageError> {
    let text = text.trim().trim_end_matches('/');
    let id = text.rsplit('/').next().unwrap_or(text);
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
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnlinkForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    link: String,
}

/// Removes the link the form named, as `DELETE /api/v1/transfer-links/{id}` does.
pub(super) async fn unlink(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<UnlinkForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    let link: TransferLinkId = form
        .link
        .parse()
        .map_err(|_| PageError::invalid("the form could not be read"))?;
    state
        .ledger
        .remove_transfer_link(&actor, link)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)))
}

#[cfg(test)]
mod tests {
    use coffer_domain::ids::TransactionId;

    use super::other_leg;

    #[test]
    fn the_other_leg_is_read_from_an_id_or_its_pages_address() {
        let id = TransactionId::new();
        assert_eq!(other_leg(&id.to_string()).unwrap(), id);
        assert_eq!(
            other_leg(&format!(" https://coffer.example/transactions/{id}/ ")).unwrap(),
            id
        );
        assert!(other_leg("https://coffer.example/transactions/").is_err());
        assert!(other_leg("").is_err());
    }
}
