//! Connections, sync rate limits and provider account binding (spec §6.2,
//! §6.11): credentials sealed before anything is stored, removal that
//! serializes with a running sync, limits that refuse with the right
//! `Retry-After`, and binding that turns held records into transactions.
//! Providers are scripted with the testkit's `FakeProvider`.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod binding;
mod credentials;
mod removal;
mod sync_limits;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::{AccountScope, Action, Denied, DenyReason};
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::SyncError;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{
    ConnectInput, FetchResult, ProviderAccountData, ProviderKind, ProviderTransaction,
};
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_server::vault::SealedCredentials;
use coffer_store::{
    AuditEvent, AuditFilter, NewSyncRun, PageRequest, ProviderAccountRecord, TransactionFilter,
    TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::Timestamp;
use jiff::civil::date;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt as _;

/// The credentials of every test connection. Recognisable, so a leak shows
/// up in a text search.
const PLAINTEXT: &str = "https://user:SECRET-CREDENTIAL-MARKER@bridge.example/simplefin";
const SECRET_PART: &str = "SECRET-CREDENTIAL-MARKER";

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

impl World {
    fn now(&self) -> Timestamp {
        self.server.clock.now()
    }

    /// Fetches `accounts` from the provider in a run queued directly, so no
    /// rate limit applies.
    async fn sync_once(&self, accounts: Vec<ProviderAccountData>) {
        self.fake.push(Ok(FetchResult {
            accounts,
            connection_errors: Vec::new(),
            skipped: 0,
            cursor: None,
        }));
        let mut tx = self.server.store.begin().await.unwrap();
        let run = tx
            .insert_sync_run(NewSyncRun {
                connection: self.connection,
                trigger: SyncTrigger::Manual,
                requested_by: Some((self.alice.member, None)),
                queued_at: self.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.server.sync.run(run.id).await.unwrap();
    }

    async fn account(&self, name: &str, currency: &str) -> AccountId {
        self.server
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

    async fn provider_accounts(&self) -> Vec<ProviderAccountRecord> {
        self.server
            .store
            .list_provider_accounts(Some(self.connection))
            .await
            .unwrap()
    }

    async fn provider_account(&self, provider_account_id: &str) -> ProviderAccountRecord {
        self.provider_accounts()
            .await
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

    async fn audit(&self) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter::default(),
                &PageRequest {
                    limit: 200,
                    ..PageRequest::default()
                },
            )
            .await
            .unwrap()
            .items
    }

    async fn audit_actions(&self, action: &str) -> Vec<AuditEvent> {
        self.audit()
            .await
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }

    async fn token(&self, scope: Scope) -> SecretToken {
        let (_, secret) = self
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
        secret
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn sealed(&self) -> Option<Vec<u8>> {
        self.server
            .store
            .get_connection(self.connection)
            .await
            .unwrap()
            .credentials
    }

    /// Whether `sealed` opens to `expected`; never prints either.
    fn opens_to(&self, id: ConnectionId, sealed: Vec<u8>, expected: &str) -> bool {
        self.server
            .vault
            .open(id, &SealedCredentials::from(sealed))
            .is_ok_and(|opened| opened.expose_secret() == expected)
    }
}

fn setup_token(text: &str) -> ConnectInput {
    ConnectInput::SimpleFinSetupToken(SecretString::from(text))
}

fn txn(provider_id: &str, value: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: provider_id.into(),
        posted_on: Some(date(2027, 1, 10)),
        transacted_on: None,
        amount: DecimalAmount::parse(value).unwrap(),
        amount_raw: value.to_owned(),
        description: "COFFEE SHOP PURCHASE".into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

fn reported(
    provider_account_id: &str,
    currency: &str,
    kind_hint: Option<AccountKind>,
    transactions: Vec<ProviderTransaction>,
) -> ProviderAccountData {
    ProviderAccountData {
        provider_account_id: provider_account_id.into(),
        name: "Everyday savings".into(),
        institution: Some("Example Bank".into()),
        currency: currency.into(),
        kind_hint,
        balance: None,
        transactions,
        removed: Vec::new(),
        coverage: Coverage::Delta,
        error: None,
        unusable: Vec::new(),
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Whether `err` is a denial of `action` for `reason`.
fn denied(err: &SyncError, action: Action, reason: DenyReason) -> bool {
    matches!(err, SyncError::Denied(d) if *d == Denied { action, reason })
}

const RENEWED: &str = "https://user:RENEWED-CREDENTIAL@bridge.example/simplefin";
