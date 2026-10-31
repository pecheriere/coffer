//! The sync engine: a queued run fetches from its connection's provider
//! with a computed window and the household's `today`, applies every
//! provider account in its own store transaction, and records the outcome
//! on the connection, its provider accounts and the run (spec §6.2).
//! Providers are scripted with the testkit's `FakeProvider`.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;

use coffer_domain::accounts::{AccountKind, BalanceSource, ConnectionLabel};
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::ids::{AccountId, ConnectionId, SyncRunId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::engine::SyncRunView;
use coffer_server::sync::provider::{
    FetchResult, ProviderAccountData, ProviderError, ProviderKind, ProviderTransaction,
};
use coffer_server::testkit::{
    FakeProvider, INJECTED_FAILURE_RECORD_ID, TestServer, captured_log_text,
};
use coffer_store::{
    AuditFilter, ConnectionStatus, NewSyncRun, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, TransactionFilter, TransactionRecord,
};
use jiff::civil::{Date, date};
use jiff::{SignedDuration, Span, Timestamp};
use secrecy::ExposeSecret as _;

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
        posted_on,
        transacted_on: None,
        amount: amount(value),
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
    }
}

fn fetched(accounts: Vec<ProviderAccountData>) -> FetchResult {
    FetchResult {
        accounts,
        connection_errors: Vec::new(),
        cursor: None,
    }
}

#[tokio::test]
async fn first_window_is_max_then_overlap() {
    let world = world().await;
    for _ in 0..3 {
        world.fake.push(Ok(fetched(Vec::new())));
    }

    let first = world.sync().await;
    assert_eq!(first.status, SyncRunStatus::Succeeded);
    let first_today = world.today();
    let first_success = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap()
        .last_success_at
        .unwrap();

    world.server.clock.advance(SignedDuration::from_hours(36));
    world.sync().await;

    world
        .server
        .clock
        .advance(SignedDuration::from_hours(200 * 24));
    world.sync().await;

    let requests = world.fake.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].window_start,
        Some(first_today.checked_sub(Span::new().days(90)).unwrap()),
        "the first run asks for the provider's maximum window"
    );
    let success_day = first_success
        .to_zoned(world.server.config.timezone.clone())
        .date();
    assert_eq!(
        requests[1].window_start,
        Some(success_day.checked_sub(Span::new().days(14)).unwrap()),
        "later runs overlap the last success"
    );
    assert_eq!(
        requests[2].window_start,
        Some(world.days_ago(90)),
        "after a long gap the window is clamped to the maximum"
    );
    assert!(requests.iter().all(|request| request.cursor.is_none()));
}

#[tokio::test]
async fn fetch_request_carries_household_today() {
    let mut server = TestServer::with_env(&[("COFFER_TIMEZONE", "Pacific/Auckland")]).await;
    let fake = server.use_fake_provider();
    let world = world_on(server, fake).await;
    world
        .server
        .clock
        .set("2026-10-31T12:00:00Z".parse::<Timestamp>().unwrap());
    world.fake.push(Ok(fetched(Vec::new())));
    world.sync().await;
    let requests = world.fake.requests();
    assert_eq!(requests[0].today, date(2026, 11, 1));
    assert_eq!(requests[0].timezone, "Pacific/Auckland");
}

#[tokio::test]
async fn new_provider_accounts_appear_unbound_and_hold_records() {
    let world = world().await;
    let start = world.days_ago(90);
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-new",
        "USD",
        start,
        vec![
            txn("t-1", world.days_ago(3), "-12.50"),
            txn("t-2", world.days_ago(2), "100"),
        ],
    )])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Succeeded);
    let record = world.provider_account("acct-new").await;
    assert_eq!(record.account, None, "new provider accounts are unbound");
    assert_eq!(record.name, "Account acct-new");
    assert_eq!(record.institution.as_deref(), Some("Example Bank"));
    assert_eq!(record.currency_raw, "USD");
    assert_eq!(record.kind_hint, Some(AccountKind::Checking));
    assert_eq!(record.error, None);

    let mut tx = world.server.store.begin().await.unwrap();
    let held = tx.held_sources(record.id).await.unwrap();
    drop(tx);
    let mut ids: Vec<_> = held
        .iter()
        .map(|source| (source.provider_txn_id.as_str(), source.fields.amount_minor))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, [("t-1", -1250), ("t-2", 10_000)]);
    assert!(
        held.iter().all(|source| source.transaction.is_none()),
        "held records belong to no transaction"
    );
}

#[tokio::test]
async fn currency_problem_isolated_to_one_account() {
    let world = world().await;
    let usd_b = world.account("B", "USD").await;
    let usd_c = world.account("C", "USD").await;
    world.bound("acct-b", "EUR", usd_b).await;
    world.bound("acct-c", "USD", usd_c).await;
    let start = world.days_ago(90);
    let day = world.days_ago(1);
    world.fake.push(Ok(fetched(vec![
        provider_account(
            "acct-a",
            "https://custom",
            start,
            vec![txn("a-1", day, "-1")],
        ),
        provider_account("acct-b", "EUR", start, vec![txn("b-1", day, "-2")]),
        provider_account("acct-c", "USD", start, vec![txn("c-1", day, "-3")]),
    ])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);

    let a = world.provider_account("acct-a").await;
    assert!(
        a.error.is_some(),
        "a non-ISO currency is the account's error"
    );
    assert_eq!(a.currency_raw, "https://custom");
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(
        tx.held_sources(a.id).await.unwrap().len(),
        0,
        "no record held"
    );
    drop(tx);

    let b = world.provider_account("acct-b").await;
    assert!(
        b.error.is_some(),
        "a currency other than the bound account's is the account's error"
    );
    assert_eq!(world.transactions(usd_b).await.len(), 0, "no transaction");

    let c = world.provider_account("acct-c").await;
    assert_eq!(c.error, None);
    let committed = world.transactions(usd_c).await;
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].amount_minor, -300);

    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert!(connection.last_success_at.is_some());
}

#[tokio::test]
async fn account_failure_rolls_back_only_that_account() {
    let world = world().await;
    let failing = world.account("Failing", "USD").await;
    let healthy = world.account("Healthy", "USD").await;
    world.bound("acct-x", "USD", failing).await;
    world.bound("acct-y", "USD", healthy).await;
    world.server.inject_source_insert_failure().await;
    let start = world.days_ago(90);
    let day = world.days_ago(1);
    let mut x = provider_account(
        "acct-x",
        "USD",
        start,
        vec![
            txn("x-1", day, "-1"),
            txn("x-2", day, "-2"),
            txn(INJECTED_FAILURE_RECORD_ID, day, "-3"),
        ],
    );
    x.balance = Some((day, amount("10")));
    world.fake.push(Ok(fetched(vec![
        x,
        provider_account("acct-y", "USD", start, vec![txn("y-1", day, "-4")]),
    ])));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    assert!(
        world.transactions(failing).await.is_empty(),
        "nothing of the failed account is committed"
    );
    assert_eq!(
        world.server.store.latest_balance(failing).await.unwrap(),
        None
    );
    let x = world.provider_account("acct-x").await;
    assert!(x.error.is_some(), "the failure is the account's error");
    assert_eq!(
        x.last_balance, None,
        "the account's balance rolled back too"
    );
    let mut tx = world.server.store.begin().await.unwrap();
    assert_eq!(
        tx.held_sources(x.id).await.unwrap().len(),
        0,
        "no record held"
    );
    for id in ["x-1", "x-2"] {
        assert!(
            tx.find_source(world.connection, id)
                .await
                .unwrap()
                .is_none(),
            "{id}"
        );
    }
    drop(tx);
    assert_eq!(world.transactions(healthy).await.len(), 1);
    assert_eq!(world.provider_account("acct-y").await.error, None);
}

#[tokio::test]
async fn connection_errors_alongside_data_are_partial() {
    let world = world().await;
    let mut result = fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        vec![txn("t-1", world.days_ago(1), "-5")],
    )]);
    result.connection_errors = vec![
        "Connection to Example Bank may need attention\u{1b}[0m".into(),
        "Another institution is slow".into(),
    ];
    world.fake.push(Ok(result));

    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Active);
    assert_eq!(
        connection.last_error.as_deref(),
        Some("Connection to Example Bank may need attention[0m; Another institution is slow")
    );
    assert!(connection.last_success_at.is_some());
    let mut tx = world.server.store.begin().await.unwrap();
    let record = world.provider_account("acct-1").await;
    assert_eq!(tx.held_sources(record.id).await.unwrap().len(), 1);
    drop(tx);

    // A clean run clears the error.
    world.fake.push(Ok(fetched(Vec::new())));
    assert_eq!(world.sync().await.status, SyncRunStatus::Succeeded);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.last_error, None);
}

#[tokio::test]
async fn reauth_marks_connection() {
    let world = world().await;
    world.fake.push(Err(ProviderError::ReauthRequired));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Failed);
    assert!(run.finished_at.is_some());
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::ReauthRequired);
    assert_eq!(connection.last_success_at, None);
    assert!(connection.last_attempt_at.is_some());
}

#[tokio::test]
async fn provider_error_message_sanitized() {
    let world = world().await;
    world.fake.push(Err(ProviderError::Unavailable(
        "failed for https://u:p@bridge.example/x\u{7}".into(),
    )));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Failed);
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.status, ConnectionStatus::Error);
    assert_eq!(
        connection.last_error.as_deref(),
        Some("failed for https://***@bridge.example/x")
    );
    assert_eq!(
        run.detail.as_deref(),
        Some("failed for https://***@bridge.example/x")
    );
}

#[tokio::test]
async fn balance_snapshot_uses_provider_balance_date() {
    let world = world().await;
    let account = world.account("Checking", "USD").await;
    world.bound("acct-1", "USD", account).await;
    let described = world.days_ago(2);
    let mut data = provider_account("acct-1", "USD", world.days_ago(90), Vec::new());
    data.balance = Some((described, amount("1234.56")));
    world.fake.push(Ok(fetched(vec![data])));
    world.sync().await;

    let snapshot = world
        .server
        .store
        .latest_balance(account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.on, described,
        "dated by the day the balance describes"
    );
    assert_eq!(snapshot.balance_minor, 123_456);
    assert_eq!(snapshot.source, BalanceSource::Sync);
    let record = world.provider_account("acct-1").await;
    let raw = record.last_balance.unwrap();
    assert_eq!((raw.on, raw.amount_raw.as_str()), (described, "1234.56"));

    // A balance dated after the household's today is dated today.
    let tomorrow = world.today().checked_add(Span::new().days(1)).unwrap();
    let mut data = provider_account("acct-1", "USD", world.days_ago(90), Vec::new());
    data.balance = Some((tomorrow, amount("99")));
    world.fake.push(Ok(fetched(vec![data])));
    world.sync().await;
    let snapshot = world
        .server
        .store
        .latest_balance(account)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (snapshot.on, snapshot.balance_minor),
        (world.today(), 9_900)
    );
}

#[tokio::test]
async fn credentials_never_logged() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        Vec::new(),
    )])));
    // A provider error quoting the credentials, as a careless one might.
    world.fake.push(Err(ProviderError::Invalid(format!(
        "request to {PLAINTEXT} failed"
    ))));

    let succeeded = world.sync().await;
    let failed = world.sync().await;
    assert_eq!(failed.status, SyncRunStatus::Failed);

    let requests = world.fake.requests();
    assert!(
        requests
            .iter()
            .all(|request| request.credentials.expose_secret() == PLAINTEXT),
        "the provider receives the opened credentials"
    );

    let logs = captured_log_text();
    assert!(
        logs.contains(&world.connection.to_string()),
        "the log captured the sync of the connection"
    );
    assert!(
        !logs.contains(SECRET_PART),
        "the credentials are never logged"
    );

    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    let last_error = connection.last_error.unwrap_or_default();
    assert!(
        !last_error.contains(SECRET_PART),
        "last_error lacks the credentials"
    );
    assert!(
        !connection
            .credentials
            .unwrap()
            .windows(SECRET_PART.len())
            .any(|window| window == SECRET_PART.as_bytes()),
        "the stored credentials are sealed"
    );
    for run in [succeeded, failed] {
        let detail = run.detail.clone().unwrap_or_default();
        assert!(
            !detail.contains(SECRET_PART),
            "run detail lacks the credentials"
        );
        assert!(
            !format!("{run:?}").contains(SECRET_PART),
            "run view lacks the credentials"
        );
    }
    let audit = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap();
    assert!(
        audit
            .items
            .iter()
            .all(|event| !format!("{event:?}").contains(SECRET_PART)),
        "the audit log lacks the credentials"
    );
}

#[tokio::test]
async fn run_view_reports_attribution_and_records() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![provider_account(
        "acct-1",
        "USD",
        world.days_ago(90),
        vec![
            txn("t-1", world.days_ago(1), "-1"),
            txn("", world.days_ago(1), "-1"),
        ],
    )])));
    let run = world.sync().await;
    assert_eq!(run.connection, world.connection);
    assert_eq!(run.trigger, SyncTrigger::Manual);
    let requested_by = run.requested_by.clone().unwrap();
    assert_eq!(requested_by.member, Some(world.alice.member));
    assert_eq!(requested_by.agent, None);
    assert!(run.started_at.is_some() && run.finished_at.is_some());
    assert_eq!(
        run.status,
        SyncRunStatus::Partial,
        "a skipped malformed record makes the run partial"
    );
    let detail = run.detail.clone().unwrap();
    assert!(
        detail.contains("1 skipped"),
        "the detail counts skipped records"
    );
    let stored = world.server.store.get_sync_run(run.id).await.unwrap();
    assert_eq!(
        (stored.status, stored.detail.as_deref(), stored.finished_at),
        (run.status, Some(detail.as_str()), run.finished_at)
    );
    let connection = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(connection.provider, ProviderKind::SimpleFin.as_str());
    assert_eq!(
        connection.label,
        ConnectionLabel::parse("Family bank").unwrap()
    );

    assert!(
        world.server.sync.run(run.id).await.is_err(),
        "a finished run does not run again"
    );
    assert!(world.server.sync.run(SyncRunId::new()).await.is_err());
}

#[tokio::test]
async fn invalid_provider_account_id_leaves_a_trace() {
    let world = world().await;
    world.fake.push(Ok(fetched(vec![
        provider_account("", "USD", world.days_ago(90), Vec::new()),
        provider_account("acct-ok", "USD", world.days_ago(90), Vec::new()),
    ])));
    let run = world.sync().await;
    assert_eq!(run.status, SyncRunStatus::Partial);
    let detail = run.detail.unwrap();
    assert!(
        detail.starts_with("2 accounts, 1 with errors"),
        "the run counts the rejected account"
    );
    let logs = captured_log_text();
    let connection = world.connection.to_string();
    assert!(
        logs.lines().any(|line| {
            line.starts_with("WARN")
                && line.contains("provider account with an invalid id skipped")
                && line.contains(&connection)
        }),
        "a warning names the connection"
    );
}

#[tokio::test]
async fn captured_logs_include_span_fields() {
    let _server = TestServer::new().await;
    let marker = format!("span-marker-{}", ConnectionId::new());
    let span = tracing::info_span!("probe", secret = %marker, later = tracing::field::Empty);
    let recorded = format!("recorded-marker-{}", ConnectionId::new());
    span.record("later", recorded.as_str());
    let logs = captured_log_text();
    assert!(logs.contains(&marker), "span fields are captured");
    assert!(
        logs.contains(&recorded),
        "fields recorded later are captured"
    );
}
