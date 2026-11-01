//! The connection API and the sync scheduler (spec §6.2, §6.11): listing
//! connections, triggering a sync that the worker then performs, the
//! limits a trigger meets, listing runs, removing a connection, and the
//! recovery of runs a restart interrupted. Providers are scripted with the
//! testkit's `FakeProvider`.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AgentTokenId, ConnectionId, SyncRunId};
use coffer_domain::money::DecimalAmount;
use coffer_domain::policy::AccountScope;
use coffer_domain::sync::{SyncRunStatus, SyncTrigger};
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::connections::NewLinkedAccountInput;
use coffer_server::sync::provider::{
    FetchResult, ProviderAccountData, ProviderError, ProviderKind, ProviderTransaction,
};
use coffer_server::sync::scheduler;
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::{
    AuditFilter, ConnectionStatus, ConnectionStatusChange, NewSyncRun, PageRequest, SyncRunRecord,
    TransactionFilter,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower::ServiceExt as _;

const PLAINTEXT: &str = "https://user:SECRET-CREDENTIAL-MARKER@bridge.example/simplefin";

/// How long a test waits for the worker before failing.
const WAIT: Duration = Duration::from_secs(10);

struct World {
    server: TestServer,
    fake: Arc<FakeProvider>,
    alice: Actor,
    connection: ConnectionId,
}

/// Admin "alice" with a connection seeded under her name.
async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
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

/// A running scheduler and the means to stop it.
struct Running {
    stop: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.handle)
            .await
            .unwrap()
            .unwrap();
    }
}

impl World {
    fn spawn_scheduler(&self) -> Running {
        let (stop, stopped) = oneshot::channel::<()>();
        let handle = scheduler::spawn(self.server.app_state(), async {
            let _ = stopped.await;
        });
        Running { stop, handle }
    }

    fn script_one_account(&self) {
        self.fake.push(Ok(FetchResult {
            accounts: vec![ProviderAccountData {
                provider_account_id: "acct-1".into(),
                name: "Everyday savings".into(),
                institution: Some("Example Bank".into()),
                currency: "USD".into(),
                kind_hint: Some(AccountKind::Savings),
                balance: None,
                transactions: vec![ProviderTransaction {
                    provider_id: "t-1".into(),
                    posted_on: Some(date(2027, 1, 10)),
                    transacted_on: None,
                    amount: DecimalAmount::parse("-12.50").unwrap(),
                    amount_raw: "-12.50".to_owned(),
                    description: "COFFEE SHOP PURCHASE".into(),
                    payee: None,
                    memo: None,
                    pending: false,
                    replaces_provider_id: None,
                }],
                removed: Vec::new(),
                coverage: Coverage::Delta,
                error: None,
                unusable: Vec::new(),
            }],
            connection_errors: Vec::new(),
            cursor: None,
        }));
    }

    async fn token(&self, scope: Scope) -> (AgentTokenId, SecretToken) {
        let (view, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        (view.id, secret)
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
    ) -> (StatusCode, Option<String>, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|value| value.to_str().unwrap().to_owned());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            retry_after,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn run_record(&self, id: SyncRunId) -> SyncRunRecord {
        self.server.store.get_sync_run(id).await.unwrap()
    }

    /// Waits until the run has finished, then returns it.
    async fn finished(&self, id: SyncRunId) -> SyncRunRecord {
        tokio::time::timeout(WAIT, async {
            loop {
                let run = self.run_record(id).await;
                if run.finished_at.is_some() {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    /// Queues a run of the connection directly, bypassing the limits.
    async fn queue_run(&self) -> SyncRunId {
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
        run.id
    }

    async fn transaction_count(&self) -> usize {
        self.server
            .store
            .list_transactions(
                &TransactionFilter {
                    scope: AccountScope::All,
                    accounts: Vec::new(),
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
            .len()
    }
}

#[tokio::test]
async fn connections_list_shows_status_and_errors() {
    let world = world().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        world.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::Error,
            last_error: Some("the bridge is unavailable".to_owned()),
            attempted_at: Some(world.server.clock.now()),
            succeeded_at: None,
            at: world.server.clock.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_, reader) = world.token(Scope::Read).await;

    let (status, _, list) = world
        .call(Method::GET, "/api/v1/connections", &reader)
        .await;

    assert_eq!(status, StatusCode::OK);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], json!(world.connection.to_string()));
    assert_eq!(items[0]["status"], json!("error"));
    assert_eq!(items[0]["last_error"], json!("the bridge is unavailable"));
    assert_eq!(items[0]["label"], json!("Family bank"));
    assert!(items[0]["next_sync_at"].is_string());
    assert!(items[0].get("credentials").is_none());
}

#[tokio::test]
async fn sync_trigger_attributed_to_agent() {
    let world = world().await;
    world.script_one_account();
    let (token_id, writer) = world.token(Scope::Write).await;

    let (status, _, run) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{}/sync", world.connection),
            &writer,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(run["status"], json!("queued"));
    assert_eq!(run["trigger"], json!("manual"));
    assert_eq!(run["requested_by"]["agent"], json!(token_id.to_string()));
    let run_id: SyncRunId = run["id"].as_str().unwrap().parse().unwrap();

    let audit = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items;
    let requests: Vec<_> = audit
        .iter()
        .filter(|event| event.action == "sync.request")
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(token_id),
        }
    );

    let running = world.spawn_scheduler();
    let finished = world.finished(run_id).await;
    running.stop().await;
    assert_eq!(finished.status, SyncRunStatus::Succeeded);
    assert_eq!(world.fake.requests().len(), 1);

    let (status, _, runs) = world
        .call(
            Method::GET,
            &format!("/api/v1/connections/{}/sync-runs", world.connection),
            &writer,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = runs["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], json!(run_id.to_string()));
    assert_eq!(items[0]["status"], json!("succeeded"));
    assert_eq!(runs["next"], Value::Null);
}

#[tokio::test]
async fn sync_runs_page_with_a_cursor() {
    let world = world().await;
    let first = world.queue_run().await;
    world.server.sync.run(first).await.unwrap();
    let second = world.queue_run().await;
    let (_, reader) = world.token(Scope::Read).await;
    let path = format!("/api/v1/connections/{}/sync-runs", world.connection);

    let (status, _, page) = world
        .call(Method::GET, &format!("{path}?limit=1"), &reader)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], json!(second.to_string()));
    let next = page["next"].as_str().unwrap().to_owned();

    let (status, _, page) = world
        .call(
            Method::GET,
            &format!("{path}?limit=1&after={next}"),
            &reader,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["id"], json!(first.to_string()));
    assert_eq!(page["next"], Value::Null);

    let (status, _, problem) = world
        .call(Method::GET, &format!("{path}?after=bogus"), &reader)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["code"], json!("invalid_cursor"));

    let (status, _, problem) = world
        .call(
            Method::GET,
            &format!("/api/v1/connections/{}/sync-runs", ConnectionId::new()),
            &reader,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["code"], json!("not_found"));
}

#[tokio::test]
async fn second_trigger_within_min_interval_429() {
    let world = world().await;
    let (_, writer) = world.token(Scope::Write).await;
    let path = format!("/api/v1/connections/{}/sync", world.connection);

    let (status, _, run) = world.call(Method::POST, &path, &writer).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Still queued: in progress.
    let (status, retry_after, problem) = world.call(Method::POST, &path, &writer).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("sync_in_progress"));
    assert_eq!(retry_after, None);

    let run_id: SyncRunId = run["id"].as_str().unwrap().parse().unwrap();
    world.server.sync.run(run_id).await.unwrap();
    world
        .server
        .clock
        .advance(jiff::SignedDuration::from_mins(10));

    // The fake provider's minimum interval is an hour.
    let (status, retry_after, problem) = world.call(Method::POST, &path, &writer).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(problem["code"], json!("sync_rate_limited"));
    assert_eq!(retry_after.as_deref(), Some("3000"));
}

#[tokio::test]
async fn read_token_cannot_trigger_sync() {
    let world = world().await;
    let (_, reader) = world.token(Scope::Read).await;

    let (status, _, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/connections/{}/sync", world.connection),
            &reader,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("forbidden"));

    let (status, _, _) = world
        .call(
            Method::DELETE,
            &format!("/api/v1/connections/{}", world.connection),
            &reader,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let runs = world
        .server
        .store
        .list_sync_runs(world.connection, &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(runs.items.len(), 0);
}

#[tokio::test]
async fn delete_connection_keeps_transactions() {
    let world = world().await;
    world.script_one_account();
    let run = world.queue_run().await;
    world.server.sync.run(run).await.unwrap();
    let Ok(mut listed) = world
        .server
        .store
        .list_provider_accounts(Some(world.connection))
        .await
    else {
        panic!("the provider accounts can be listed");
    };
    let provider_account = listed.remove(0);
    world
        .server
        .sync
        .create_account_and_bind(
            &world.alice,
            provider_account.id,
            NewLinkedAccountInput {
                name: None,
                kind: None,
                liquidity: None,
                include_in_net_worth: true,
                include_in_forecast: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(world.transaction_count().await, 1);
    let (_, writer) = world.token(Scope::Write).await;
    let path = format!("/api/v1/connections/{}", world.connection);

    // The provider fails to give the credentials up: nothing changes.
    world
        .fake
        .fail_next_disconnect(ProviderError::Unavailable("bridge down".into()));
    let (status, _, problem) = world.call(Method::DELETE, &path, &writer).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(problem["code"], json!("provider_error"));
    let kept = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(kept.status, ConnectionStatus::Active);
    assert!(kept.credentials.is_some());

    let (status, _, body) = world.call(Method::DELETE, &path, &writer).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    let removed = world
        .server
        .store
        .get_connection(world.connection)
        .await
        .unwrap();
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert!(removed.credentials.is_none());
    assert_eq!(world.transaction_count().await, 1);

    let (status, _, problem) = world.call(Method::DELETE, &path, &writer).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("conflict"));
}

#[tokio::test]
async fn interrupted_run_marked_failed_on_start() {
    let world = world().await;
    let interrupted = world.queue_run().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.start_sync_run(interrupted, world.server.clock.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let running = world.spawn_scheduler();
    let run = world.finished(interrupted).await;
    running.stop().await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn scheduler_queues_due_connections_and_drains_them() {
    let world = world().await;
    world.script_one_account();

    // Never attempted: due at once.
    let running = world.spawn_scheduler();
    let run = tokio::time::timeout(WAIT, async {
        loop {
            let runs = world
                .server
                .store
                .list_sync_runs(world.connection, &PageRequest::default())
                .await
                .unwrap()
                .items;
            if let Some(run) = runs.into_iter().find(|run| run.finished_at.is_some()) {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    running.stop().await;

    assert_eq!(run.trigger, SyncTrigger::Schedule);
    assert_eq!(run.requested_by, None);
    assert_eq!(run.status, SyncRunStatus::Succeeded);
}

/// A run whose provider panics is failed, and the worker goes on with the
/// next queued run.
#[tokio::test]
async fn panicking_run_is_failed_and_the_queue_drains() {
    let world = world().await;
    world.fake.panic_next_fetch();
    let panicking = world.queue_run().await;
    world.script_one_account();
    let next = world.queue_run().await;

    let running = world.spawn_scheduler();
    let failed = world.finished(panicking).await;
    let drained = world.finished(next).await;
    running.stop().await;

    assert_eq!(failed.status, SyncRunStatus::Failed);
    assert_eq!(drained.status, SyncRunStatus::Succeeded);
}

/// Runs already queued when the scheduler starts are performed.
#[tokio::test]
async fn runs_queued_before_start_are_drained() {
    let world = world().await;
    world.script_one_account();
    let queued = world.queue_run().await;

    let running = world.spawn_scheduler();
    let run = world.finished(queued).await;
    running.stop().await;

    assert_eq!(run.trigger, SyncTrigger::Manual);
    assert_eq!(run.status, SyncRunStatus::Succeeded);
}

/// Shutdown drops a run still fetching; it stays running until the next
/// start marks it failed as interrupted.
#[tokio::test]
async fn shutdown_aborts_a_running_run() {
    let world = world().await;
    let gate = world.fake.hold_next_fetch();
    world.script_one_account();
    let held = world.queue_run().await;

    let running = world.spawn_scheduler();
    tokio::time::timeout(WAIT, gate.entered()).await.unwrap();
    running.stop().await;
    gate.release();

    let left = world.run_record(held).await;
    assert_eq!(left.status, SyncRunStatus::Running);
    assert_eq!(world.transaction_count().await, 0, "nothing was applied");

    let restarted = world.spawn_scheduler();
    let run = world.finished(held).await;
    restarted.stop().await;
    assert_eq!(run.status, SyncRunStatus::Failed);
    assert_eq!(run.detail.as_deref(), Some("interrupted"));
}
