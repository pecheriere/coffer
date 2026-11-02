//! The daily table: every change that moves a number moves it in its own
//! transaction, and whatever way a row was reached, a full rebuild reaches the same.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, LiquidityClass};
use coffer_domain::fx::{FxRate, FxSource, RateObservation};
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::{AccountId, ConnectionId, MemberId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_domain::net_worth::DailyValue;
use coffer_store::{
    BalanceSnapshot, KeptBalance, NetWorthState, NewAccount, NewConnection, NewMember,
    ProviderAccountUpsert, Store,
};
use jiff::Timestamp;
use jiff::civil::{Date, date};
use tempfile::TempDir;

mod incremental;
mod reads;
mod rebuild;

/// What `mark` leaves in a row: any other balance was written after it.
const MARK: i64 = -999;

fn at() -> Timestamp {
    Timestamp::from_second(1_800_000_000).unwrap()
}

fn cur(code: &str) -> Currency {
    Currency::parse(code).unwrap()
}

/// A day of October 2026.
fn oct(day: i8) -> Date {
    date(2026, 10, day)
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

async fn holding(store: &Store, code: &str) -> AccountId {
    holding_with(store, code, true).await
}

async fn holding_with(store: &Store, code: &str, included: bool) -> AccountId {
    let id = AccountId::new();
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(NewAccount {
        id,
        name: AccountName::parse("Holding").unwrap(),
        kind: AccountKind::Checking,
        currency: cur(code),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: included,
        include_in_forecast: true,
        created_at: at(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

fn snapshot(account: AccountId, on: Date, minor: i64, source: BalanceSource) -> BalanceSnapshot {
    BalanceSnapshot {
        account,
        on,
        balance_minor: minor,
        source,
        recorded_at: at(),
    }
}

/// Writes a manual snapshot in its own transaction.
async fn snap(store: &Store, account: AccountId, on: Date, minor: i64) {
    let mut tx = store.begin().await.unwrap();
    tx.upsert_balance(snapshot(account, on, minor, BalanceSource::Manual))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn observations(rates: &[(Date, &str)]) -> Vec<RateObservation> {
    rates
        .iter()
        .map(|&(on, rate)| RateObservation {
            on,
            rate: rate.parse::<FxRate>().unwrap(),
        })
        .collect()
}

/// Stores rates in their own transaction.
async fn fx(store: &Store, code: &str, rates: &[(Date, &str)]) {
    let mut tx = store.begin().await.unwrap();
    tx.upsert_fx_rates(cur(code), FxSource::Ecb, &observations(rates), at())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn show_in(store: &Store, code: &str) {
    let mut tx = store.begin().await.unwrap();
    tx.set_display_currency(cur(code)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn rebuild(store: &Store, through: Date) -> u64 {
    let mut tx = store.begin().await.unwrap();
    let written = tx.rebuild_net_worth(through).await.unwrap();
    tx.commit().await.unwrap();
    written
}

async fn fill(store: &Store, through: Date) -> u64 {
    let mut tx = store.begin().await.unwrap();
    let written = tx.fill_net_worth(through).await.unwrap();
    tx.commit().await.unwrap();
    written
}

async fn dump(store: &Store) -> Vec<DailyValue> {
    store.daily_values_for_tests().await.unwrap()
}

async fn state(store: &Store) -> NetWorthState {
    store.net_worth_state().await.unwrap()
}

/// `account`'s rows, oldest first.
async fn rows_of(store: &Store, account: AccountId) -> Vec<DailyValue> {
    dump(store)
        .await
        .into_iter()
        .filter(|row| row.account == account)
        .collect()
}

async fn row_on(store: &Store, account: AccountId, on: Date) -> DailyValue {
    rows_of(store, account)
        .await
        .into_iter()
        .find(|row| row.on == on)
        .unwrap()
}

/// Overwrites `account`'s balances with [`MARK`], so a row that shows anything
/// else afterwards was written, and a row still showing it was not.
async fn mark(store: &Store, account: AccountId) {
    store
        .execute_unchecked_for_tests(
            "UPDATE account_daily_values SET balance_minor = -999 WHERE account_id = ?",
            &account.to_string(),
        )
        .await
        .unwrap();
}

/// The days of `account`'s rows that were written since [`mark`].
async fn written_days(store: &Store, account: AccountId) -> Vec<Date> {
    rows_of(store, account)
        .await
        .into_iter()
        .filter(|row| row.balance_minor != MARK)
        .map(|row| row.on)
        .collect()
}

/// The days from `first` to `last`, of October 2026.
fn days(first: i8, last: i8) -> Vec<Date> {
    (first..=last).map(oct).collect()
}

async fn connection(store: &Store) -> ConnectionId {
    let mut tx = store.begin().await.unwrap();
    let owner = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("alice").unwrap(),
            display_name: DisplayName::parse("Alice").unwrap(),
            role: Role::Admin,
            created_at: at(),
        })
        .await
        .unwrap();
    let created = tx
        .insert_connection(NewConnection {
            provider: "testbank",
            label: ConnectionLabel::parse("Family bank").unwrap(),
            owner: owner.id,
            credentials: vec![9; 40],
            created_at: at(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    created.id
}

/// A provider account of `connection`, unbound, holding `kept` in `code`.
async fn kept(
    store: &Store,
    connection: ConnectionId,
    feed: &str,
    kept: &[(Date, i64, &str)],
) -> ProviderAccountId {
    let mut tx = store.begin().await.unwrap();
    let provider_account = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection,
            provider_account_id: feed.into(),
            name: "Feed".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: at(),
        })
        .await
        .unwrap()
        .id;
    for &(on, balance_minor, code) in kept {
        tx.keep_balance(KeptBalance {
            provider_account,
            on,
            balance_minor,
            currency: cur(code),
            recorded_at: at(),
        })
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    provider_account
}

/// A bind's history, in its own transaction.
async fn bring_in(store: &Store, feed: ProviderAccountId, account: AccountId, code: &str) -> u32 {
    let mut tx = store.begin().await.unwrap();
    let brought = tx
        .bring_in_kept_balances(feed, account, cur(code))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    brought.written
}
