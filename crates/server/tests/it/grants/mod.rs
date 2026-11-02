//! Per-account grants of agent tokens, enforced through the real router
//! (spec §8.2): a token with grants sees only the accounts they name, at
//! their levels; a hidden account answers exactly like a missing one; the
//! actions of the policy's rule table need an `all` grant; and no response
//! reveals a hidden account.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
mod endpoints;
mod hidden_accounts;
mod management;
mod proptests;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{Description, TransactionFlag, TransactionOrigin};
use coffer_server::http::router;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::transactions::NewManualTransactionInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::{FakeProvider, TestServer};
use coffer_store::{
    AuditFilter, ConnectionStatus, ConnectionStatusChange, ImportedFields, NewConnection,
    NewSource, NewTransaction, PageRequest, ProviderAccountUpsert,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt as _;

const LEVELS: [AccountLevel; 3] = [
    AccountLevel::Balances,
    AccountLevel::Transactions,
    AccountLevel::Write,
];

struct World {
    server: TestServer,
    fake: Arc<FakeProvider>,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let (server, fake) = TestServer::with_fake_provider().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    World {
        server,
        fake,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

fn grant(target: GrantTarget, level: AccountLevel) -> TokenGrant {
    TokenGrant { target, level }
}

fn on(account: AccountId, level: AccountLevel) -> TokenGrant {
    grant(GrantTarget::Account(account), level)
}

impl World {
    /// A token of alice's with `scope` and `grants`.
    async fn token(&self, scope: Scope, grants: Vec<TokenGrant>) -> (AgentTokenId, SecretToken) {
        let (record, secret) = self
            .server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap();
        (record.id, secret)
    }

    async fn account(&self, name: &str) -> AccountId {
        self.server
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Checking,
                    currency: Currency::parse("USD").unwrap(),
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

    async fn transaction(&self, account: AccountId, day: i8, description: &str) -> TransactionId {
        self.server
            .ledger
            .create_manual_transaction(
                &self.alice,
                NewManualTransactionInput {
                    account,
                    posted_on: date(2026, 10, day),
                    minor_units: -1000,
                    description: description.into(),
                    payee: None,
                    memo: None,
                    notes: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    async fn balance(&self, account: AccountId) {
        self.server
            .ledger
            .set_manual_balance(&self.alice, account, date(2026, 10, 1), 50_000)
            .await
            .unwrap();
    }

    /// A connection reporting one provider account per entry of
    /// `accounts`: its name, and the account it is bound to.
    async fn connection(&self, accounts: &[(&str, Option<AccountId>)]) -> ConnectionId {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        let connection = tx
            .insert_connection(NewConnection {
                provider: ProviderKind::SimpleFin.as_str(),
                label: ConnectionLabel::parse("Family bank").unwrap(),
                owner: self.alice.member,
                credentials: vec![1, 2, 3],
                created_at: now,
            })
            .await
            .unwrap();
        for (n, (name, bound)) in accounts.iter().enumerate() {
            let record = tx
                .upsert_provider_account(ProviderAccountUpsert {
                    connection: connection.id,
                    provider_account_id: format!("acct-{n}"),
                    name: (*name).to_owned(),
                    institution: None,
                    currency_raw: "USD".into(),
                    kind_hint: Some(AccountKind::Checking),
                    balance: None,
                    seen_at: now,
                })
                .await
                .unwrap();
            if let Some(account) = bound {
                tx.bind_provider_account(record.id, *account).await.unwrap();
            }
        }
        tx.commit().await.unwrap();
        connection.id
    }

    /// Records `error` as the connection's last error.
    async fn fail_connection(&self, connection: ConnectionId, error: &str) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.update_connection_status(
            connection,
            ConnectionStatusChange {
                status: ConnectionStatus::Error,
                last_error: Some(error.to_owned()),
                attempted_at: Some(now),
                succeeded_at: None,
                at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// A transaction imported into `account` with one source record from the
    /// provider account of `connection` bound to it (the first one if none
    /// is). Returns its id and the source record's.
    async fn imported(
        &self,
        connection: ConnectionId,
        account: AccountId,
        description: &str,
    ) -> (TransactionId, String) {
        let now = self.server.clock.now();
        let accounts = self
            .server
            .store
            .list_provider_accounts(Some(connection))
            .await
            .unwrap();
        let provider_account = accounts
            .iter()
            .find(|provider_account| provider_account.account == Some(account))
            .unwrap_or(&accounts[0])
            .id;
        let posted_on = date(2026, 10, 3);
        let description = Description::from_provider(description);
        let mut tx = self.server.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Imported,
                account,
                posted_on,
                transacted_on: None,
                pending: false,
                amount_minor: -2500,
                currency: Currency::parse("USD").unwrap(),
                raw_description: description.clone(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: now,
            })
            .await
            .unwrap();
        let source = tx
            .insert_source(NewSource {
                connection,
                provider_account,
                provider_txn_id: format!("txn-{}", record.id),
                transaction: Some(record.id),
                fields: ImportedFields {
                    posted_on,
                    transacted_on: None,
                    pending: false,
                    amount_minor: -2500,
                    description,
                    payee: None,
                    memo: None,
                },
                amount_raw: "-25.00".to_owned(),
                seen_at: now,
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (record.id, s(source.id))
    }

    /// Flags `transaction` as a possible duplicate of `candidate`. The store
    /// refuses a pair across accounts, and these tests seed some on purpose,
    /// so the pair goes in through the store's test-only unchecked insert.
    async fn flag_duplicate(&self, transaction: TransactionId, candidate: TransactionId) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.set_flag(transaction, TransactionFlag::PossibleDuplicate, true)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.server
            .store
            .add_duplicate_candidate_unchecked_for_tests(transaction, candidate)
            .await
            .unwrap();
    }

    async fn audit_count(&self) -> usize {
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
            .len()
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
        key: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
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
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, text) = self.send(method, uri, token, body.as_ref(), None).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, uri: &str, token: &SecretToken) -> (StatusCode, Value) {
        self.call(Method::GET, uri, token, None).await
    }
}

/// The `id`s of a list response's `items`, or of a bare array.
fn ids(body: &Value) -> Vec<String> {
    body.get("items")
        .unwrap_or(body)
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

fn s(id: impl ToString + Copy) -> String {
    id.to_string()
}
