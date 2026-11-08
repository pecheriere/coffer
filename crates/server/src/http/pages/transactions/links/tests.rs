//! What the transfer card says and offers, for each actor and state of a link.

use coffer_domain::ids::{AccountId, TransactionId, TransferLinkId};
use coffer_domain::transactions::TransactionFlag;
use coffer_domain::transfer_rate::{RateCheck, RateMismatch};
use jiff::civil::date;

use super::{
    LinkState, NO_CANDIDATES, OTHER_TAKEN_BACK, REVISED, SPLIT_LEG, card, other_leg, rate_words,
};
use crate::http::pages::transactions::fixtures::{
    by_member, names, split_in_two, transaction, usd,
};
use crate::ledger::transactions::TransactionView;
use crate::ledger::transfer_links::{TransferCandidates, TransferLegView, TransferLinkView};

const UNLINK: &str = "Remove the link";
const PASTE: &str = "Link it with another transaction";

fn leg(of: &TransactionView) -> TransferLegView {
    TransferLegView {
        transaction: of.id,
        account: of.account,
        amount: of.amount,
        posted_on: of.posted_on,
        pending: of.pending,
        flags: of.flags.clone(),
    }
}

/// The link between `outflow` and `inflow`, as the service reads it.
fn link(outflow: &TransactionView, inflow: &TransactionView) -> LinkState {
    LinkState::Linked(Box::new(TransferLinkView {
        id: TransferLinkId::new(),
        outflow: leg(outflow),
        inflow: leg(inflow),
        difference: Some(usd(outflow.amount.minor_units + inflow.amount.minor_units)),
        one_movement: true,
        one_movement_in_reports: true,
        rate_check: None,
        created_by: by_member(),
    }))
}

fn unlinked(candidates: Vec<TransactionView>) -> LinkState {
    LinkState::Unlinked(TransferCandidates { items: candidates })
}

fn render(
    of: &TransactionView,
    state: &LinkState,
    readable: &[AccountId],
    writable: &[AccountId],
) -> String {
    card(of, state, &names(readable, writable), "token").into_string()
}

#[test]
fn the_other_leg_is_read_from_an_id_or_its_pages_address() {
    let id = TransactionId::new();
    assert_eq!(other_leg(&id.to_string()).unwrap(), id);
    for address in [
        format!(" https://coffer.example/transactions/{id}/ "),
        format!("https://coffer.example/transactions/{id}?from=review"),
        format!("https://coffer.example/transactions/{id}/#transfer"),
        format!("/transactions/{id}#transfer?x=1"),
    ] {
        assert_eq!(other_leg(&address).unwrap(), id);
    }
    assert!(other_leg("https://coffer.example/transactions/").is_err());
    assert!(other_leg("https://coffer.example/transactions/?id=1").is_err());
    assert!(other_leg("").is_err());
}

#[test]
fn a_link_says_what_left_and_arrived_and_offers_its_removal() {
    let (checking, savings) = (AccountId::new(), AccountId::new());
    let outflow = transaction(checking, -50_000);
    let inflow = transaction(savings, 49_500);
    let both = [checking, savings];

    let page = render(&outflow, &link(&outflow, &inflow), &both, &both);

    assert!(page.contains("500.00 USD left an account on 2 October 2026, and 495.00 USD reached"));
    assert!(page.contains("5.00 USD less arrived than left"));
    assert!(page.contains(UNLINK));
    assert!(!page.contains(", pending"));
    assert!(!page.contains(OTHER_TAKEN_BACK));
}

#[test]
fn a_link_says_when_the_other_leg_is_pending_or_was_taken_back() {
    let (checking, savings) = (AccountId::new(), AccountId::new());
    let outflow = transaction(checking, -50_000);
    let mut inflow = transaction(savings, 50_000);
    inflow.pending = true;
    inflow.flags = vec![TransactionFlag::SourceRemoved];
    let both = [checking, savings];

    let page = render(&outflow, &link(&outflow, &inflow), &both, &both);

    assert!(page.contains("reached an account on 2 October 2026, pending."));
    assert!(page.contains(OTHER_TAKEN_BACK));
}

#[test]
fn legs_a_bank_turned_the_same_way_are_said_as_they_are() {
    let (checking, savings) = (AccountId::new(), AccountId::new());
    let outflow = transaction(checking, -50_000);
    let inflow = transaction(savings, -50_000);
    let both = [checking, savings];

    let page = render(&inflow, &link(&outflow, &inflow), &both, &both);

    assert!(page.contains(REVISED));
    assert!(page.contains("-500.00 USD in an account on 2 October 2026, and -500.00 USD in"));
    assert!(!page.contains(" left "));
    assert!(!page.contains("arrived than left"));
    assert!(page.contains(UNLINK), "the link can still be removed");
}

#[test]
fn a_link_isnt_offered_for_removal_without_write_on_both_accounts() {
    let (checking, savings) = (AccountId::new(), AccountId::new());
    let outflow = transaction(checking, -50_000);
    let inflow = transaction(savings, 50_000);
    let state = link(&outflow, &inflow);
    let both = [checking, savings];

    assert!(!render(&outflow, &state, &both, &[checking]).contains(UNLINK));
    assert!(!render(&outflow, &state, &both, &[]).contains(UNLINK));
}

#[test]
fn candidates_are_offered_to_link_only_where_the_actor_writes_both_accounts() {
    let (checking, savings, shared) = (AccountId::new(), AccountId::new(), AccountId::new());
    let this = transaction(checking, -50_000);
    let mut writable = transaction(savings, 50_000);
    writable.payee = Some("Into savings".to_owned());
    let mut read_only = transaction(shared, 50_000);
    read_only.payee = Some("Into shared".to_owned());
    let state = unlinked(vec![writable, read_only]);
    let all = [checking, savings, shared];

    let page = render(&this, &state, &all, &[checking, savings]);
    assert!(page.contains("aria-label=\"Link with Into savings\""));
    assert!(!page.contains("aria-label=\"Link with Into shared\""));
    assert!(page.contains("Into shared"), "still named");
    assert!(page.contains(PASTE));

    let page = render(&this, &state, &all, &[savings, shared]);
    assert!(!page.contains("aria-label=\"Link with"));
    assert!(!page.contains(PASTE));
    assert!(!page.contains("<form"));
}

#[test]
fn with_no_candidate_it_says_so_without_naming_any_account() {
    let checking = AccountId::new();
    let this = transaction(checking, -50_000);

    let page = render(&this, &unlinked(Vec::new()), &[checking], &[checking]);

    assert!(page.contains(NO_CANDIDATES));
    assert!(!page.contains("an account"));
    assert!(page.contains(PASTE));
}

#[test]
fn a_split_transaction_says_why_it_cant_be_linked_and_offers_nothing() {
    let checking = AccountId::new();
    let this = split_in_two(transaction(checking, -50_000));

    let page = render(&this, &unlinked(Vec::new()), &[checking], &[checking]);

    assert!(page.contains(SPLIT_LEG));
    assert!(!page.contains("<form"));
}

#[test]
fn a_rate_mismatch_names_the_tolerance_the_check_uses() {
    let words = rate_words(&RateCheck::Mismatch(RateMismatch {
        outflow: usd(-11_000),
        inflow: usd(10_000),
        expected_inflow: usd(10_800),
        rate_on: date(2026, 10, 2),
        manual_rate: true,
    }));
    assert!(words.contains("2 October 2026, entered by hand"));
    assert!(words.contains("more than 5% away"));
}
