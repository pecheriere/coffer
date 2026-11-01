//! The sync engine: a queued run fetches from its connection's provider
//! with a computed window and the household's `today`, applies every
//! provider account in its own store transaction, and records the outcome
//! on the connection, its provider accounts and the run (spec §6.2).
//! Providers are scripted with the testkit's `FakeProvider`.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod deadline;
mod provider_accounts;
mod records;
mod reporting;
mod windows;

use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::engine::SyncRunView;
use coffer_server::sync::provider::{
    FetchResult, ProviderAccountData, ProviderKind, ProviderTransaction,
};
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::{
    NewSyncRun, PageRequest, ProviderAccountRecord, ProviderAccountUpsert, TransactionFilter,
    TransactionRecord,
};
use jiff::Span;
use jiff::civil::Date;
use std::sync::Arc;

/// The sealed credentials of every test connection. Recognisable, so a leak
/// shows up in a text search.
const PLAINTEXT: &str = "https://user:SECRET-CREDENTIAL-MARKER@bridge.example/simplefin";
const SECRET_PART: &str = "SECRET-CREDENTIAL-MARKER";

struct World {
    server: TestServer,
    fake: Arc<FakeProvider>,
    alice: Actor,
    connection: ConnectionId,
}

async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
    world_on(server, fake).await
}

async fn world_on(server: TestServer, fake: Arc<FakeProvider>) -> World {
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let connection = server
        .seed_connection(ProviderKind::SimpleFin, "Family bank", member, PLAINTEXT)
        .await;
    World {
        server,
        fake,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
        connection,
    }
}

impl World {
    fn today(&self) -> Date {
        self.server
            .clock
            .now()
            .to_zoned(self.server.config.timezone.clone())
            .date()
    }

    fn days_ago(&self, days: i64) -> Date {
        self.today().checked_sub(Span::new().days(days)).unwrap()
    }

    /// Queues a manual run of the connection and runs it.
    async fn sync(&self) -> SyncRunView {
        let mut tx = self.server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: self.connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.alice.member, None)),
                queued_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.server.sync.run(run.id).await.unwrap()
    }

    async fn account(&self, name: &str, currency: &str) -> AccountId {
        self.server
            .app_state()
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Checking,
                    currency: Currency::parse(currency).unwrap(),
                    liquidity: None,
                    include_in_net_worth: true,
                    include_in_forecast: true,
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    /// A provider account of the connection, bound to `account`.
    async fn bound(&self, provider_account_id: &str, currency: &str, account: AccountId) {
        let mut tx = self.server.store.begin().await.unwrap();
        let provider_account = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection: self.connection,
                provider_account_id: provider_account_id.into(),
                name: provider_account_id.into(),
                institution: None,
                currency_raw: currency.into(),
                kind_hint: None,
                balance: None,
                seen_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.bind_provider_account(provider_account.id, account)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn provider_account(&self, provider_account_id: &str) -> ProviderAccountRecord {
        self.server
            .store
            .list_provider_accounts(Some(self.connection))
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.provider_account_id == provider_account_id)
            .unwrap()
    }

    async fn transactions(&self, account: AccountId) -> Vec<TransactionRecord> {
        self.server
            .store
            .list_transactions(
                &TransactionFilter {
                    scope: AccountScope::All,
                    accounts: vec![account],
                    from: None,
                    to: None,
                    review: None,
                    assignee: None,
                    categories: Vec::new(),
                    uncategorized: false,
                    pending: None,
                    flag: None,
                    min_minor: None,
                    max_minor: None,
                    text: None,
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
    }
}

fn amount(text: &str) -> DecimalAmount {
    DecimalAmount::parse(text).unwrap()
}

fn txn(provider_id: &str, posted_on: Date, value: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: provider_id.into(),
        posted_on: Some(posted_on),
        transacted_on: None,
        amount: amount(value),
        amount_raw: value.to_owned(),
        description: "COFFEE SHOP PURCHASE".into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

fn provider_account(
    provider_account_id: &str,
    currency: &str,
    start: Date,
    transactions: Vec<ProviderTransaction>,
) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: provider_account_id.into(),
        name: format!("Account {provider_account_id}"),
        institution: Some("Example Bank".into()),
        currency: currency.into(),
        kind_hint: Some(AccountKind::Checking),
        balance: None,
        transactions,
        removed: Vec::new(),
        coverage: Coverage::Window { start },
        error: None,
        unusable: Vec::new(),
    }
}

fn fetched(accounts: Vec<ProviderAccountData>) -> FetchResult {
    FetchResult {
        accounts,
        connection_errors: Vec::new(),
        cursor: None,
    }
}
