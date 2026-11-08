//! What the duplicates card says and offers, for each actor.

use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId, SourceRecordId};
use coffer_domain::transactions::{TransactionFlag, TransactionOrigin};
use jiff::Timestamp;
use jiff::civil::date;

use super::{CANDIDATES_GONE, DISCARD, card};
use crate::http::pages::transactions::fixtures::{names, transaction};
use crate::ledger::transactions::{SourceView, TransactionView};

const MERGE: &str = "Keep this one, delete that one";
const DISMISS: &str = "It isn't a duplicate";
const UNMERGE: &str = "Move it out";

fn source(pending: bool) -> SourceView {
    SourceView {
        id: SourceRecordId::new(),
        connection: ConnectionId::new(),
        provider_account: ProviderAccountId::new(),
        pending,
        posted_on: date(2026, 10, 2),
        amount_raw: "-12.50".to_owned(),
        first_seen_at: Timestamp::UNIX_EPOCH,
    }
}

/// An imported transaction in `account` flagged as a possible duplicate of `candidates`, with
/// two bank records merged into it.
fn flagged(account: AccountId, candidates: &[TransactionView]) -> TransactionView {
    let mut this = transaction(account, -1_250);
    this.origin = TransactionOrigin::Imported;
    this.flags = vec![TransactionFlag::PossibleDuplicate];
    this.duplicate_candidates = candidates.iter().map(|candidate| candidate.id).collect();
    this.sources = vec![source(true), source(false)];
    this
}

fn render(of: &TransactionView, candidates: &[TransactionView], writable: &[AccountId]) -> String {
    card(of, candidates, &names(&[of.account], writable), "token").into_string()
}

#[test]
fn an_actor_who_changes_the_account_gets_every_form() {
    let checking = AccountId::new();
    let candidates = [transaction(checking, -1_250)];
    let this = flagged(checking, &candidates);

    let page = render(&this, &candidates, &[checking]);

    assert!(page.contains("This may be the same purchase as:"));
    assert!(page.contains(MERGE));
    assert!(page.contains(DISCARD));
    assert!(page.contains(DISMISS));
    assert!(page.contains(UNMERGE));
}

#[test]
fn an_actor_who_only_reads_the_account_sees_what_is_there_and_no_form() {
    let checking = AccountId::new();
    let candidates = [transaction(checking, -1_250)];
    let this = flagged(checking, &candidates);

    let page = render(&this, &candidates, &[]);

    assert!(page.contains("This may be the same purchase as:"));
    assert!(page.contains("Corner shop"), "the candidate is named");
    assert!(
        page.contains("-12.50 as the bank wrote it"),
        "the records are listed"
    );
    assert!(!page.contains("<form"));
}

#[test]
fn a_candidate_in_an_account_the_actor_cant_change_offers_no_merge() {
    let (checking, shared) = (AccountId::new(), AccountId::new());
    let candidates = [transaction(shared, -1_250)];
    let this = flagged(checking, &candidates);

    let page = render(&this, &candidates, &[checking]);

    assert!(!page.contains(MERGE));
    assert!(page.contains(DISMISS));
}

#[test]
fn candidates_gone_since_are_said_gone() {
    let checking = AccountId::new();
    let gone = [transaction(checking, -1_250)];
    let this = flagged(checking, &gone);

    let page = render(&this, &[], &[checking]);

    assert!(page.contains(CANDIDATES_GONE));
    assert!(!page.contains("This may be the same purchase as:"));
    assert!(page.contains(DISMISS), "the mark can still be cleared");
}

#[test]
fn a_transaction_neither_flagged_nor_merged_has_no_card() {
    let checking = AccountId::new();
    let this = transaction(checking, -1_250);
    assert_eq!(render(&this, &[], &[checking]), "");
}
