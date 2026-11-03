//! Rules through the real router and the sync's apply step (spec §8.13): what they
//! fill on arrival, what a manual run does, how they meet splits, merges, goal links
//! and archived categories, and what a token with grants sees of them.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
mod arrival;
mod manual;
mod meeting;
mod validation;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{CategoryId, ConnectionId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::policy::TokenGrant;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::CategoryKind;
use coffer_server::http::router;
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_server::testkit::TestServer;
use coffer_store::{
    AccountRecord, AuditEvent, AuditFilter, NewConnection, PageRequest, ProviderAccountRecord,
    ProviderAccountUpsert, TransactionRecord,
};
use http_body_util::BodyExt as _;
use jiff::civil::date;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    ledger: Arc<Ledger>,
    alice: Actor,
    write: SecretToken,
}

/// A connection with one provider account bound to an account.
struct Feed {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    account: AccountRecord,
}

async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let ledger = server.app_state().ledger;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, write) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("claude").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    World {
        server,
        ledger,
        alice,
        write,
    }
}

impl World {
    async fn token(&self, scope: Scope, grants: Vec<TokenGrant>) -> SecretToken {
        self.server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("narrow").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap()
            .1
    }

    async fn account(&self, name: &str) -> AccountRecord {
        let view = self
            .ledger
            .create_account(
                &self.alice,
                NewAccountInput {
                    name: name.into(),
                    kind: AccountKind::Checking,
                    currency: Currency::parse("USD").unwrap(),
                    liquidity: None,
                    include_in_net_worth: Some(true),
                    include_in_forecast: Some(true),
                    belongs_to: None,
                    beneficiary: None,
                    opening_balance: None,
                },
                None,
            )
            .await
            .unwrap();
        self.server.store.get_account(view.id).await.unwrap()
    }

    /// A new connection whose provider account is bound to `account`.
    async fn feed(&self, account: &AccountRecord) -> Feed {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        let connection = tx
            .insert_connection(NewConnection {
                provider: "testbank",
                label: ConnectionLabel::parse("Bank").unwrap(),
                owner: self.alice.member,
                credentials: vec![1, 2, 3],
                created_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection: connection.id,
                provider_account_id: "acct-1".into(),
                name: "Everyday".into(),
                institution: None,
                currency_raw: "USD".into(),
                kind_hint: Some(AccountKind::Checking),
                balance: None,
                seen_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx
            .bind_provider_account(provider_account.id, account.id)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        Feed {
            connection: connection.id,
            provider_account,
            account: account.clone(),
        }
    }

    /// Applies `transactions` as one fetch, with `removed` gone.
    async fn sync_removing(
        &self,
        feed: &Feed,
        transactions: Vec<NormalizedTransaction>,
        removed: Vec<String>,
    ) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        apply_account_batch(
            &mut tx,
            now,
            feed.connection,
            AccountBatch {
                provider_account: feed.provider_account.clone(),
                account: Some(feed.account.clone()),
                transactions,
                removed,
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn sync(&self, feed: &Feed, transactions: Vec<NormalizedTransaction>) {
        self.sync_removing(feed, transactions, Vec::new()).await;
    }

    /// The new transaction of the record `id`, arrived now.
    async fn arrive(&self, feed: &Feed, id: &str, amount_minor: i64, text: &str) -> TransactionId {
        self.sync(feed, vec![record(id, amount_minor, text)]).await;
        self.holding(feed, id).await
    }

    async fn holding(&self, feed: &Feed, provider_id: &str) -> TransactionId {
        let mut tx = self.server.store.begin().await.unwrap();
        let source = tx
            .find_source(feed.connection, provider_id)
            .await
            .unwrap()
            .unwrap();
        source.transaction.unwrap()
    }

    async fn stored(&self, id: TransactionId) -> TransactionRecord {
        self.server.store.get_transaction(id).await.unwrap()
    }

    async fn category_of_kind(&self, name: &str, kind: CategoryKind) -> CategoryId {
        let group = self
            .ledger
            .create_category_group(
                &self.alice,
                CategoryGroupInput {
                    name: format!("{name} group"),
                    position: None,
                },
                None,
            )
            .await
            .unwrap();
        self.ledger
            .create_category(
                &self.alice,
                CategoryInput {
                    group: group.id,
                    name: name.into(),
                    kind,
                    position: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    async fn category(&self, name: &str) -> CategoryId {
        self.category_of_kind(name, CategoryKind::Expense).await
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

    /// Creates a rule with the write token and answers its id.
    async fn rule(&self, body: Value) -> String {
        let (status, rule) = self
            .call(Method::POST, "/api/v1/rules", &self.write, Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{rule}");
        rule["id"].as_str().unwrap().to_owned()
    }

    /// A rule filing what contains `pattern` under `category`.
    async fn filing(&self, pattern: &str, category: CategoryId) -> String {
        self.rule(json!({"match_kind": "contains", "pattern": pattern, "category": category}))
            .await
    }

    async fn run(&self, rule: &str) -> Value {
        let (status, body) = self
            .call(
                Method::POST,
                &format!("/api/v1/rules/{rule}/apply"),
                &self.write,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn dry_run(&self, rule: &str) -> Value {
        let (status, body) = self
            .call(
                Method::GET,
                &format!("/api/v1/rules/{rule}/dry-run"),
                &self.write,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn patch_transaction(&self, id: TransactionId, body: Value) -> Value {
        let (status, answer) = self
            .call(
                Method::PATCH,
                &format!("/api/v1/transactions/{id}"),
                &self.write,
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    async fn read_transaction(&self, id: TransactionId) -> Value {
        let (status, answer) = self
            .call(
                Method::GET,
                &format!("/api/v1/transactions/{id}"),
                &self.write,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    /// Every audit event with this action, oldest first.
    async fn events(&self, action: &str) -> Vec<AuditEvent> {
        let mut found = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .server
                .store
                .list_audit(
                    &AuditFilter::default(),
                    &PageRequest {
                        after: after.clone(),
                        limit: 200,
                    },
                )
                .await
                .unwrap();
            found.extend(page.items.into_iter().filter(|e| e.action == action));
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        found.reverse();
        found
    }

    async fn second_member(&self) -> MemberId {
        let mut tx = self.server.store.begin().await.unwrap();
        let member = tx
            .insert_member(coffer_store::NewMember {
                id: MemberId::new(),
                username: coffer_domain::identity::Username::parse("bob").unwrap(),
                display_name: coffer_domain::identity::DisplayName::parse("Bob").unwrap(),
                role: Role::Member,
                created_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        member.id
    }
}

fn record(id: &str, amount_minor: i64, description: &str) -> NormalizedTransaction {
    NormalizedTransaction {
        provider_id: id.into(),
        posted_on: date(2026, 10, 10),
        date_reported: true,
        transacted_on: None,
        amount_minor,
        amount_raw: format!("{amount_minor}"),
        description: description.into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
        dating: None,
    }
}

fn pending(id: &str, amount_minor: i64, description: &str) -> NormalizedTransaction {
    NormalizedTransaction {
        pending: true,
        ..record(id, amount_minor, description)
    }
}

/// The value of `field` an audit event recorded after the change.
fn after(event: &AuditEvent, field: &str) -> Option<coffer_domain::audit::AuditValue> {
    event
        .changes
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, change)| change.after.clone())
}

fn text(value: &impl ToString) -> coffer_domain::audit::AuditValue {
    coffer_domain::audit::AuditValue::Text(value.to_string())
}
