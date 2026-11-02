//! Planned items through the real router (spec §6.7, §8.9 R3): what they
//! hold, what is refused, and that one hinting at an account the token can't
//! see at `transactions` answers exactly like a missing one.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod crud;
mod goal_links;
mod grants;
mod validation;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryGroupId, CategoryId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::transactions::CategoryKind;
use coffer_server::http::router;
use coffer_server::ledger::accounts::{AccountPatch, NewAccountInput};
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ITEMS: &str = "/api/v1/plan-items";

struct World {
    server: TestServer,
    alice: Actor,
    /// A token of alice's without grants, which may do anything.
    admin: SecretToken,
    /// An expense category.
    expense: CategoryId,
}

/// Admin "alice", signed in with a passkey, and one expense category.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, admin) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("admin").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut world = World {
        server,
        alice,
        admin,
        expense: CategoryId::new(),
    };
    world.expense = world.category(CategoryKind::Expense).await.1;
    world
}

fn on(account: AccountId, level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(account),
        level,
    }
}

fn all(level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::All,
        level,
    }
}

fn item_uri(id: impl std::fmt::Display) -> String {
    format!("{ITEMS}/{id}")
}

/// An outflow of 42.00 USD on the first of every month.
fn recurring(category: CategoryId) -> Value {
    json!({
        "name": "Water bill",
        "category": category,
        "amount": {"minor_units": 4200, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "recurring", "every": 1, "unit": "months",
                     "anchor_on": "2027-01-01"},
    })
}

/// A one-off outflow of 900.00 USD.
fn one_off(category: CategoryId) -> Value {
    json!({
        "name": "New boiler",
        "category": category,
        "amount": {"minor_units": 90000, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "one_off", "on": "2027-03-15"},
    })
}

/// A goal of 3000.00 USD.
fn goal(category: CategoryId) -> Value {
    json!({
        "name": "Summer trip",
        "category": category,
        "amount": {"minor_units": 300_000, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "goal", "start_on": "2027-01-01", "due_on": "2027-07-01"},
    })
}

/// `body` with `field` set to `value`.
fn with(mut body: Value, field: &str, value: Value) -> Value {
    body[field] = value;
    body
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

    async fn close_account(&self, account: AccountId) {
        self.server
            .ledger
            .update_account(
                &self.alice,
                account,
                AccountPatch {
                    closed: Some(true),
                    ..AccountPatch::default()
                },
            )
            .await
            .unwrap();
    }

    /// A category of `kind` in a group of its own.
    async fn category(&self, kind: CategoryKind) -> (CategoryGroupId, CategoryId) {
        let group = self
            .server
            .ledger
            .create_category_group(
                &self.alice,
                CategoryGroupInput {
                    name: "Group".into(),
                    position: None,
                },
                None,
            )
            .await
            .unwrap()
            .id;
        let category = self
            .server
            .ledger
            .create_category(
                &self.alice,
                CategoryInput {
                    group,
                    name: "Category".into(),
                    kind,
                    position: None,
                },
                None,
            )
            .await
            .unwrap()
            .id;
        (group, category)
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
        body: Option<&Value>,
    ) -> (StatusCode, Value) {
        let (status, text) = self.send(method, uri, token, body, None).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Creates an item with `token`, which must succeed; returns its id.
    async fn create_with(&self, token: &SecretToken, body: &Value) -> String {
        let (status, answer) = self.call(Method::POST, ITEMS, token, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        answer["id"].as_str().unwrap().to_owned()
    }

    /// Creates an item as alice's admin token; returns its id.
    async fn create(&self, body: &Value) -> String {
        self.create_with(&self.admin, body).await
    }

    /// Posts `body`, which must be refused with `status`; returns the
    /// problem's `detail`.
    async fn refused(&self, body: &Value, status: StatusCode) -> String {
        let (got, answer) = self
            .call(Method::POST, ITEMS, &self.admin, Some(body))
            .await;
        assert_eq!(got, status);
        answer["detail"].as_str().unwrap_or_default().to_owned()
    }

    /// Patches item `id` with `body`, which must be refused with `status`;
    /// returns the problem's `detail`.
    async fn patch_refused(&self, id: &str, body: &Value, status: StatusCode) -> String {
        let uri = item_uri(id);
        let (got, answer) = self
            .call(Method::PATCH, &uri, &self.admin, Some(body))
            .await;
        assert_eq!(got, status);
        answer["detail"].as_str().unwrap_or_default().to_owned()
    }

    /// Patches item `id` with `body`, which must succeed; returns the item.
    async fn patch(&self, id: &str, body: &Value) -> Value {
        let uri = item_uri(id);
        let (status, answer) = self
            .call(Method::PATCH, &uri, &self.admin, Some(body))
            .await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    /// The ids `token` sees at `uri`, a list of items.
    async fn listed(&self, uri: &str, token: &SecretToken) -> Vec<String> {
        let (status, answer) = self.call(Method::GET, uri, token, None).await;
        assert_eq!(status, StatusCode::OK);
        answer["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The audit entries of planned items, newest first.
    async fn audit(&self) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_type: Some("planned_item".into()),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
    }
}
