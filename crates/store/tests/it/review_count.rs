//! Counting the transactions a filter matches.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, BelongsTo, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, MemberId};
use coffer_domain::money::Currency;
use coffer_domain::policy::AccountScope;
use coffer_domain::transactions::{Description, ReviewStatus, TransactionOrigin};
use coffer_store::{
    AccountRecord, Attribution, NewAccount, NewMember, NewTransaction, ReviewChange, Store,
    TransactionFilter, UserFieldChanges,
};
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

async fn account(store: &Store, name: &str) -> AccountRecord {
    let mut tx = store.begin().await.unwrap();
    let record = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse(name).unwrap(),
            kind: AccountKind::Checking,
            currency: Currency::parse("USD").unwrap(),
            liquidity: LiquidityClass::Cash,
            liquidity_overridden: false,
            include_in_net_worth: true,
            include_in_forecast: true,
            created_at: at(0),
            belongs_to: BelongsTo::Household,
            belongs_to_overridden: false,
            beneficiary: None,
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    record
}

async fn member(store: &Store) -> MemberId {
    let mut tx = store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("alice").unwrap(),
            display_name: DisplayName::parse("Alice").unwrap(),
            role: Role::Admin,
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    member.id
}

/// Inserts a transaction in `account`, reviewed by `reviewer` when given.
async fn transaction(store: &Store, account: &AccountRecord, reviewer: Option<MemberId>) {
    let mut tx = store.begin().await.unwrap();
    let record = tx
        .insert_transaction(NewTransaction {
            origin: TransactionOrigin::Manual,
            account: account.id,
            posted_on: date(2026, 10, 2),
            transacted_on: None,
            pending: false,
            amount_minor: -500,
            currency: account.currency,
            raw_description: Description::parse("CORNER SHOP").unwrap(),
            imported_payee: None,
            memo: None,
            notes: None,
            created_at: at(5),
        })
        .await
        .unwrap();
    if let Some(member) = reviewer {
        tx.update_user_fields(
            record.id,
            UserFieldChanges {
                payee: None,
                notes: None,
                review: Some(ReviewChange::Reviewed(Attribution {
                    member: Some(member),
                    agent: None,
                    system: None,
                    rule: None,
                    at: at(6),
                })),
                assignee: None,
                at: at(6),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

fn unreviewed_in(scope: AccountScope) -> TransactionFilter {
    TransactionFilter {
        scope,
        accounts: Vec::new(),
        from: None,
        to: None,
        review: Some(ReviewStatus::Unreviewed),
        assignee: None,
        categories: Vec::new(),
        uncategorized: false,
        unmatched_transfer: false,
        rule: None,
        pending: None,
        flag: None,
        min_minor: None,
        max_minor: None,
        text: None,
    }
}

#[tokio::test]
async fn count_follows_the_filter() {
    let (_dir, store) = open().await;
    let reviewer = member(&store).await;
    let a = account(&store, "Checking").await;
    let b = account(&store, "Savings").await;
    transaction(&store, &a, None).await;
    transaction(&store, &a, None).await;
    transaction(&store, &a, Some(reviewer)).await;
    transaction(&store, &b, None).await;

    let all = unreviewed_in(AccountScope::All);
    assert_eq!(store.count_transactions(&all).await.unwrap(), 3);
    let only_a = unreviewed_in(AccountScope::Only(vec![a.id]));
    assert_eq!(store.count_transactions(&only_a).await.unwrap(), 2);
    let nothing = unreviewed_in(AccountScope::Only(Vec::new()));
    assert_eq!(store.count_transactions(&nothing).await.unwrap(), 0);
    let every_review = TransactionFilter {
        review: None,
        ..unreviewed_in(AccountScope::All)
    };
    assert_eq!(store.count_transactions(&every_review).await.unwrap(), 4);
}
