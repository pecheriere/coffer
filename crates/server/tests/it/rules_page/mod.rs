//! The rules page through the real router: the list and a rule's page, the forms that add,
//! change and delete a rule, run it and take back what it filled, each read back through the
//! API, and what is refused.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod forms;
mod refusals;
mod render;
mod runs;
mod stale;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::transactions::CategoryKind;
use coffer_server::http::router;
use coffer_server::ledger::Ledger;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::NewManualTransactionInput;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
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
    /// Alice, signed in.
    browser: Browser,
    /// A write token of Alice's, to read back and set up through the API.
    token: SecretToken,
}

async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);
    let mut auth = server.authenticator();
    let done = browser.enroll(&mut auth, &link, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK);
    let member = server.store.list_members().await.unwrap()[0].id;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("reader").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    World {
        ledger: server.app_state().ledger,
        server,
        alice,
        browser,
        token,
    }
}

impl World {
    async fn account(&self, name: &str) -> AccountId {
        self.ledger
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
            .unwrap()
            .id
    }

    async fn category(&self, name: &str) -> CategoryId {
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
                    kind: CategoryKind::Expense,
                    position: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    /// A manual transaction on `account`, posted on 10 October 2026.
    async fn transaction(&self, account: AccountId, minor: i64, text: &str) -> TransactionId {
        self.ledger
            .create_manual_transaction(
                &self.alice,
                NewManualTransactionInput {
                    account,
                    posted_on: date(2026, 10, 10),
                    minor_units: minor,
                    description: text.into(),
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

    /// Bob, a member, signed in in a browser of his own.
    async fn member_bob(&self) -> (MemberId, Browser) {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let mut bob = Browser::new(&self.server);
        let mut key = self.server.authenticator();
        let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
        assert_eq!(done.status, StatusCode::OK);
        let members = self.server.store.list_members().await.unwrap();
        let id = members
            .iter()
            .find(|member| member.username.as_str() == "bob")
            .unwrap()
            .id;
        (id, bob)
    }

    async fn api(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", self.token.expose_secret()),
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

    /// Creates a rule through the API and answers its id.
    async fn rule(&self, body: Value) -> String {
        let (status, rule) = self.api(Method::POST, "/api/v1/rules", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        rule["id"].as_str().unwrap().to_owned()
    }

    /// The rules as the API lists them.
    async fn rules(&self) -> Vec<Value> {
        let (status, list) = self.api(Method::GET, "/api/v1/rules", None).await;
        assert_eq!(status, StatusCode::OK);
        list["items"].as_array().unwrap().clone()
    }

    async fn read_transaction(&self, id: TransactionId) -> Value {
        let (status, answer) = self
            .api(Method::GET, &format!("/api/v1/transactions/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    async fn review(&self, id: TransactionId) {
        let (status, _) = self
            .api(
                Method::PATCH,
                &format!("/api/v1/transactions/{id}"),
                Some(json!({"review": "reviewed"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// Every audit event with this action, oldest first.
    async fn events(&self, action: &str) -> Vec<AuditEvent> {
        let page = self
            .server
            .store
            .list_audit(
                &AuditFilter::default(),
                &PageRequest {
                    after: None,
                    limit: 200,
                },
            )
            .await
            .unwrap();
        let mut found: Vec<AuditEvent> = page
            .items
            .into_iter()
            .filter(|event| event.action == action)
            .collect();
        found.reverse();
        found
    }
}

/// The category a transaction is filed under, read from the API's answer.
fn filed_under(transaction: &Value) -> Option<String> {
    transaction["splits"]
        .as_array()
        .unwrap()
        .first()
        .map(|split| split["category"].as_str().unwrap().to_owned())
}

/// Whether `event` was written for `member` through a session.
fn by_member(event: &AuditEvent, member: MemberId) -> bool {
    event.actor
        == AuditActor::Member {
            member,
            agent: None,
        }
}

/// The page's anti-forgery token, after loading `path`.
async fn csrf_of(browser: &mut Browser, path: &str) -> String {
    let page: Reply = browser.get(path).await;
    assert_eq!(page.status, StatusCode::OK);
    page.csrf()
}

/// `text` as an attribute value maud wrote, read back.
fn unescaped(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The `shown_*` fields the change form on `page` carries, by the name of the field each
/// stands for: what the form sends when nobody touches it.
fn shown_of(page: &Reply) -> Vec<(String, String)> {
    let form = page.between(r#"id="change""#, "</form>");
    let marker = r#"<input type="hidden" name="shown_"#;
    form.match_indices(marker)
        .map(|(at, _)| {
            let rest = &form[at + marker.len()..];
            let (name, rest) = rest.split_once('"').unwrap();
            let rest = rest.strip_prefix(r#" value=""#).unwrap();
            let (value, _) = rest.split_once('"').unwrap();
            (name.to_owned(), unescaped(value))
        })
        .collect()
}

/// The change form on `page` as a person sends it after changing `changes`: every field as
/// shown but those, with what the page showed. An empty `enabled` is an unticked box.
fn change_form(page: &Reply, changes: &[(&str, &str)]) -> Vec<(String, String)> {
    let shown = shown_of(page);
    assert_eq!(shown.len(), 10, "the form carries every field it shows");
    let mut fields = vec![("csrf".to_owned(), page.csrf())];
    for (name, value) in &shown {
        let value = changes
            .iter()
            .find(|(changed, _)| changed == name)
            .map_or(value.as_str(), |(_, value)| value);
        if !(name == "enabled" && value.is_empty()) {
            fields.push((name.clone(), value.to_owned()));
        }
    }
    for (name, value) in shown {
        fields.push((format!("shown_{name}"), value));
    }
    fields
}

/// Sends `fields`, owned, as a form.
async fn post_owned(browser: &mut Browser, path: &str, fields: &[(String, String)]) -> Reply {
    let borrowed: Vec<(&str, &str)> = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    browser.post_form(path, &borrowed).await
}
