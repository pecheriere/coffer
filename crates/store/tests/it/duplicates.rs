//! Duplicate candidates: pairs of transactions that may be one purchase.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, LiquidityClass};
use coffer_domain::ids::{AccountId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::transactions::{Description, TransactionOrigin};
use coffer_store::{NewAccount, NewTransaction, Store, StoreError, StoreTx};
use jiff::Timestamp;
use jiff::civil::date;
use tempfile::TempDir;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

async fn new_account(tx: &mut StoreTx, name: &str) -> AccountId {
    tx.insert_account(NewAccount {
        id: AccountId::new(),
        name: AccountName::parse(name).unwrap(),
        kind: AccountKind::Checking,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: at(0),
    })
    .await
    .unwrap()
    .id
}

async fn new_transaction(tx: &mut StoreTx, account: AccountId) -> TransactionId {
    tx.insert_transaction(NewTransaction {
        origin: TransactionOrigin::Imported,
        account,
        posted_on: date(2026, 10, 2),
        transacted_on: None,
        pending: false,
        amount_minor: -300,
        currency: Currency::parse("USD").unwrap(),
        raw_description: Description::parse("CORNER SHOP 0042").unwrap(),
        imported_payee: None,
        memo: None,
        notes: None,
        created_at: at(5),
    })
    .await
    .unwrap()
    .id
}

async fn candidates_of(tx: &mut StoreTx, id: TransactionId) -> Vec<TransactionId> {
    tx.get_transaction(id).await.unwrap().duplicate_candidates
}

/// A pair is recorded only within one account: one across accounts is
/// refused, the whole call with it, and the transaction stays usable.
#[tokio::test]
async fn duplicate_candidates_stay_within_one_account() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    let first = new_account(&mut tx, "Checking").await;
    let second = new_account(&mut tx, "Savings").await;
    let flagged = new_transaction(&mut tx, first).await;
    let sibling = new_transaction(&mut tx, first).await;
    let elsewhere = new_transaction(&mut tx, second).await;

    tx.add_duplicate_candidates(flagged, &[sibling])
        .await
        .unwrap();
    assert_eq!(candidates_of(&mut tx, flagged).await, [sibling]);

    assert!(
        matches!(
            tx.add_duplicate_candidates(flagged, &[elsewhere]).await,
            Err(StoreError::Invalid(_))
        ),
        "a pair across accounts is refused"
    );
    assert!(
        matches!(
            tx.add_duplicate_candidates(elsewhere, &[flagged]).await,
            Err(StoreError::Invalid(_))
        ),
        "whichever side is in the other account"
    );
    let extra = new_transaction(&mut tx, first).await;
    assert!(
        matches!(
            tx.add_duplicate_candidates(flagged, &[extra, elsewhere])
                .await,
            Err(StoreError::Invalid(_))
        ),
        "one pair across accounts refuses the batch"
    );
    assert_eq!(
        candidates_of(&mut tx, flagged).await,
        [sibling],
        "the refused calls record nothing, not even their same-account pairs"
    );
    assert_eq!(candidates_of(&mut tx, elsewhere).await, []);

    tx.add_duplicate_candidates(flagged, &[extra])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// A transaction that does not exist is still a backend failure, not a
/// cross-account pair.
#[tokio::test]
async fn duplicate_candidates_of_a_missing_transaction_fail() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    let account = new_account(&mut tx, "Checking").await;
    let present = new_transaction(&mut tx, account).await;
    assert!(matches!(
        tx.add_duplicate_candidates(present, &[TransactionId::new()])
            .await,
        Err(StoreError::Backend(_))
    ));
}
