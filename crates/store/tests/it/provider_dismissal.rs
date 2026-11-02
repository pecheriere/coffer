//! A provider account a member left unbound on purpose: the column is empty for every row that
//! existed, a refresh from a sync leaves it, binding ends it, and a bound row can't hold one.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{
    AccountKind, AccountName, BelongsTo, ConnectionLabel, LiquidityClass,
};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, MemberId};
use coffer_domain::money::Currency;
use coffer_store::{
    NewAccount, NewConnection, NewMember, ProviderAccountRecord, ProviderAccountUpsert, Store,
    StoreError,
};
use jiff::Timestamp;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

const DATABASE: &str = "coffer.db";

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

/// A store with one unbound provider account and one account to bind it to.
async fn held() -> (tempfile::TempDir, Store, ProviderAccountRecord, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join(DATABASE)).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let owner = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("alice").unwrap(),
            display_name: DisplayName::parse("Alice").unwrap(),
            role: Role::Admin,
            created_at: at(0),
        })
        .await
        .unwrap()
        .id;
    let connection = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Bank").unwrap(),
            owner,
            credentials: vec![1],
            created_at: at(0),
        })
        .await
        .unwrap();
    let account = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse("Savings").unwrap(),
            kind: AccountKind::Savings,
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
    let provider_account = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: connection.id,
            provider_account_id: "acct-1".into(),
            name: "Fund".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: at(1),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (dir, store, provider_account, account.id)
}

#[tokio::test]
async fn a_new_provider_account_is_not_dismissed() {
    let (_dir, _store, provider_account, _) = held().await;
    assert_eq!(provider_account.dismissed_at, None);
}

#[tokio::test]
async fn a_refresh_from_a_sync_keeps_the_dismissal() {
    let (_dir, store, provider_account, _) = held().await;
    let mut tx = store.begin().await.unwrap();
    tx.set_provider_account_dismissed(provider_account.id, Some(at(5)))
        .await
        .unwrap();
    let refreshed = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: provider_account.connection,
            provider_account_id: "acct-1".into(),
            name: "Fund renamed".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: at(9),
        })
        .await
        .unwrap();
    assert_eq!(refreshed.dismissed_at, Some(at(5)));
    assert_eq!(refreshed.name, "Fund renamed");
}

#[tokio::test]
async fn dismissal_is_one_way_each_time() {
    let (_dir, store, provider_account, _) = held().await;
    let mut tx = store.begin().await.unwrap();

    let not_yet = tx
        .set_provider_account_dismissed(provider_account.id, None)
        .await;
    assert!(matches!(not_yet, Err(StoreError::Conflict(_))));
    tx.set_provider_account_dismissed(provider_account.id, Some(at(2)))
        .await
        .unwrap();
    let twice = tx
        .set_provider_account_dismissed(provider_account.id, Some(at(3)))
        .await;
    assert!(matches!(twice, Err(StoreError::Conflict(_))));
    let undone = tx
        .set_provider_account_dismissed(provider_account.id, None)
        .await
        .unwrap();
    assert_eq!(undone.dismissed_at, None);
}

#[tokio::test]
async fn binding_ends_the_dismissal_and_a_bound_row_cannot_be_dismissed() {
    let (dir, store, provider_account, account) = held().await;
    let mut tx = store.begin().await.unwrap();
    tx.set_provider_account_dismissed(provider_account.id, Some(at(2)))
        .await
        .unwrap();

    let bound = tx
        .bind_provider_account(provider_account.id, account)
        .await
        .unwrap();
    assert_eq!(bound.dismissed_at, None);
    let late = tx
        .set_provider_account_dismissed(provider_account.id, Some(at(4)))
        .await;
    assert!(matches!(late, Err(StoreError::Conflict(_))));
    tx.commit().await.unwrap();

    let pool =
        SqlitePool::connect_with(SqliteConnectOptions::new().filename(dir.path().join(DATABASE)))
            .await
            .unwrap();
    let forced = sqlx::query("UPDATE provider_accounts SET dismissed_at = 1 WHERE id = ?")
        .bind(provider_account.id.to_string())
        .execute(&pool)
        .await;
    assert!(forced.is_err(), "the schema refuses a bound, dismissed row");
}
