//! Transfer links through the real router (spec §8.12): making, reading and removing one, who
//! may, what is refused, and finding the legs still without one.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
mod candidates;
mod legs;
mod links;
mod rates;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const LINKS: &str = "/api/v1/transfer-links";
const TRANSACTIONS: &str = "/api/v1/transactions";

struct World {
    server: TestServer,
    alice: Actor,
    /// A token of alice's without grants, which may do anything.
    admin: SecretToken,
    /// Alice's checking account, in USD.
    checking: AccountId,
    /// Alice's savings account, in USD.
    savings: AccountId,
}

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
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut world = World {
        server,
        alice,
        admin,
        checking: AccountId::new(),
        savings: AccountId::new(),
    };
    world.checking = world.account("Checking", "checking", "USD").await;
    world.savings = world.account("Savings", "savings", "USD").await;
    world
}

fn on(account: AccountId, level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(account),
        level,
    }
}

fn usd(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "USD"})
}

/// The body asking for a link between two transactions.
fn linking(outflow: &str, inflow: &str) -> Value {
    json!({"outflow": outflow, "inflow": inflow})
}

impl World {
    /// A token of alice's with `grants`, able to write what they allow.
    async fn token(&self, grants: Vec<TokenGrant>) -> SecretToken {
        self.server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                Scope::Write,
                None,
                grants,
            )
            .await
            .unwrap()
            .1
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
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
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// Sends `body` as alice's admin token, which must succeed; returns the answer.
    async fn admin_send(&self, method: Method, uri: &str, body: &Value) -> Value {
        let (status, answer) = self.send(method, uri, &self.admin, Some(body)).await;
        assert!(status.is_success(), "the setup must succeed: {status}");
        answer
    }

    async fn account(&self, name: &str, kind: &str, currency: &str) -> AccountId {
        let answer = self
            .admin_send(
                Method::POST,
                "/api/v1/accounts",
                &json!({"name": name, "kind": kind, "currency": currency}),
            )
            .await;
        serde_json::from_value(answer["id"].clone()).unwrap()
    }

    /// A category of `kind` in a group of its own.
    async fn category(&self, name: &str, kind: &str) -> CategoryId {
        let group = self
            .admin_send(
                Method::POST,
                "/api/v1/category-groups",
                &json!({"name": name}),
            )
            .await;
        let answer = self
            .admin_send(
                Method::POST,
                "/api/v1/categories",
                &json!({"group": group["id"], "name": name, "kind": kind}),
            )
            .await;
        serde_json::from_value(answer["id"].clone()).unwrap()
    }

    /// An uncategorized manual transaction, negative for money out; returns its id.
    async fn manual(&self, account: AccountId, posted_on: &str, minor_units: i64) -> String {
        self.admin_send(
            Method::POST,
            TRANSACTIONS,
            &json!({
                "account": account,
                "posted_on": posted_on,
                "minor_units": minor_units,
                "description": "Moved",
            }),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Sets a transaction's splits, which must be accepted.
    async fn splits(&self, transaction: &str, splits: &Value) {
        self.admin_send(
            Method::PATCH,
            &format!("{TRANSACTIONS}/{transaction}"),
            &json!({"splits": splits}),
        )
        .await;
    }

    /// 500.00 out of checking and into savings, both on the 5th. Returns (outflow, inflow).
    async fn pair(&self) -> (String, String) {
        (
            self.manual(self.checking, "2026-10-05", -50_000).await,
            self.manual(self.savings, "2026-10-05", 50_000).await,
        )
    }

    /// Links two transactions as the admin token; returns the link.
    async fn link(&self, outflow: &str, inflow: &str) -> Value {
        self.admin_send(Method::POST, LINKS, &linking(outflow, inflow))
            .await
    }

    /// The transaction as the admin token reads it.
    async fn transaction(&self, id: &str) -> Value {
        let (status, body) = self
            .send(
                Method::GET,
                &format!("{TRANSACTIONS}/{id}"),
                &self.admin,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        body
    }
}
