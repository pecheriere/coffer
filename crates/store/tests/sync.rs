//! Connections, provider accounts and their bindings.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, ConnectionLabel, LiquidityClass};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, MemberId};
use coffer_domain::money::Currency;
use coffer_store::{
    ConnectionStatus, ConnectionStatusChange, NewAccount, NewConnection, NewMember,
    ProviderAccountUpsert, RawBalance, Store, StoreError,
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

fn upsert(
    connection: coffer_domain::ids::ConnectionId,
    name: &str,
    balance: Option<RawBalance>,
    seen: i64,
) -> ProviderAccountUpsert {
    ProviderAccountUpsert {
        connection,
        provider_account_id: "acct-1".into(),
        name: name.into(),
        institution: Some("Example Bank".into()),
        currency_raw: "USD".into(),
        kind_hint: Some(AccountKind::Savings),
        balance,
        seen_at: at(seen),
    }
}

#[tokio::test]
async fn connection_lifecycle() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let mut tx = store.begin().await.unwrap();
    let created = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Family bank").unwrap(),
            owner,
            credentials: vec![9; 40],
            created_at: at(1),
        })
        .await
        .unwrap();
    assert_eq!(created.status, ConnectionStatus::Active);
    assert_eq!(created.credentials.as_deref(), Some([9; 40].as_slice()));
    assert!(
        !format!("{created:?}").contains("9, 9"),
        "Debug never shows the credentials"
    );
    tx.update_connection_status(
        created.id,
        ConnectionStatusChange {
            status: ConnectionStatus::Error,
            last_error: Some("upstream unavailable".into()),
            attempted_at: Some(at(2)),
            succeeded_at: None,
            at: at(2),
        },
    )
    .await
    .unwrap();
    tx.set_provider_state(created.id, Some("cursor-7".into()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let errored = store.get_connection(created.id).await.unwrap();
    assert_eq!(errored.status, ConnectionStatus::Error);
    assert_eq!(errored.last_error.as_deref(), Some("upstream unavailable"));
    assert_eq!(errored.last_attempt_at, Some(at(2)));
    assert_eq!(errored.last_success_at, None);
    assert_eq!(errored.provider_state.as_deref(), Some("cursor-7"));

    let mut tx = store.begin().await.unwrap();
    tx.update_connection_status(
        created.id,
        ConnectionStatusChange {
            status: ConnectionStatus::Removed,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: at(3),
        },
    )
    .await
    .unwrap();
    tx.set_connection_credentials(created.id, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let removed = store.get_connection(created.id).await.unwrap();
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert_eq!(removed.removed_at, Some(at(3)));
    assert_eq!(removed.credentials, None);
    assert_eq!(removed.last_error, None);
    assert_eq!(removed.last_attempt_at, Some(at(2)), "kept when not given");
    assert_eq!(store.list_connections().await.unwrap(), [removed]);

    let mut tx = store.begin().await.unwrap();
    let malformed = tx
        .insert_connection(NewConnection {
            provider: "Test Bank",
            label: ConnectionLabel::parse("Other").unwrap(),
            owner,
            credentials: Vec::new(),
            created_at: at(4),
        })
        .await;
    assert!(matches!(malformed, Err(StoreError::Invalid(_))));
}

#[tokio::test]
async fn provider_account_refresh_keeps_binding_and_balance() {
    let (_dir, store) = open().await;
    let owner = member(&store).await;
    let mut tx = store.begin().await.unwrap();
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
        })
        .await
        .unwrap();
    let balance = RawBalance {
        on: date(2026, 10, 30),
        amount_raw: "1250.00".into(),
    };
    let first = tx
        .upsert_provider_account(upsert(connection.id, "Savings", Some(balance.clone()), 1))
        .await
        .unwrap();
    assert_eq!(first.account, None);
    assert!(!tx.account_is_linked(account.id).await.unwrap());
    tx.bind_provider_account(first.id, account.id)
        .await
        .unwrap();
    tx.set_provider_account_error(first.id, Some("stale data".into()))
        .await
        .unwrap();
    let refreshed = tx
        .upsert_provider_account(upsert(connection.id, "Savings plus", None, 2))
        .await
        .unwrap();
    assert_eq!(refreshed.id, first.id, "the same provider account");
    assert_eq!(refreshed.name, "Savings plus");
    assert_eq!(refreshed.account, Some(account.id), "the binding stays");
    assert_eq!(
        refreshed.last_balance,
        Some(balance),
        "kept without a new one"
    );
    assert_eq!(refreshed.error.as_deref(), Some("stale data"));
    assert_eq!(refreshed.first_seen_at, at(1));
    assert_eq!(refreshed.last_seen_at, at(2));

    let rebound = tx.bind_provider_account(first.id, account.id).await;
    assert!(matches!(rebound, Err(StoreError::Conflict(_))));
    tx.commit().await.unwrap();
    assert!(store.account_is_linked(account.id).await.unwrap());
    assert_eq!(
        store
            .list_provider_accounts(Some(connection.id))
            .await
            .unwrap(),
        std::slice::from_ref(&refreshed)
    );
    assert_eq!(
        store.list_provider_accounts(None).await.unwrap(),
        [refreshed]
    );
}
