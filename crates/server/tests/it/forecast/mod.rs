//! The cash forecast and its what-if through the real router (spec §6.8, §8.9): the
//! number, what it says it doesn't know, that a what-if writes nothing, and that a
//! token short of `all` at `transactions` learns nothing.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
pub(crate) mod answer;
mod budget_month;
mod charges;
pub(crate) mod settled;
mod settlement_changes;
mod too_large;
pub(crate) mod unknowns;
mod what_if;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use jiff::Timestamp;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const FORECAST: &str = "/api/v1/forecast";
const WHAT_IF: &str = "/api/v1/forecast/what-if";
const SETTINGS: &str = "/api/v1/household/settings";
const ITEMS: &str = "/api/v1/plan-items";
const TRANSACTIONS: &str = "/api/v1/transactions";

/// The worked example's day; `today` in every world unless a test moves the clock.
pub(crate) const AS_OF: &str = "2026-11-15";
/// The balance date of the worked example's accounts, the day before `AS_OF`.
pub(crate) const BALANCE_ON: &str = "2026-11-14";

pub(crate) struct World {
    pub(crate) server: TestServer,
    pub(crate) alice: Actor,
    /// A token of alice's without grants, which may do anything.
    pub(crate) admin: SecretToken,
}

/// Admin "alice" on the day `AS_OF` in UTC, with nothing else.
pub(crate) async fn world() -> World {
    world_on(TestServer::new().await).await
}

async fn world_on(server: TestServer) -> World {
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
    server.clock.set(noon(AS_OF));
    World {
        server,
        alice,
        admin,
    }
}

fn noon(day: &str) -> Timestamp {
    format!("{day}T12:00:00Z").parse().unwrap()
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

pub(crate) fn usd(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "USD"})
}

/// A recurring monthly schedule from `anchor_on`.
fn monthly(anchor_on: &str) -> Value {
    json!({"type": "recurring", "every": 1, "unit": "months", "anchor_on": anchor_on})
}

fn one_off_on(on: &str) -> Value {
    json!({"type": "one_off", "on": on})
}

fn goal_schedule(start_on: &str, due_on: &str) -> Value {
    json!({"type": "goal", "start_on": start_on, "due_on": due_on})
}

/// An outflow named `name`, of `minor_units` USD, in `category`.
pub(crate) fn outflow(
    name: &str,
    minor_units: i64,
    category: CategoryId,
    schedule: &Value,
) -> Value {
    json!({
        "name": name,
        "category": category,
        "amount": usd(minor_units),
        "direction": "outflow",
        "schedule": schedule,
    })
}

/// `body` with `field` set to `value`.
fn with(mut body: Value, field: &str, value: Value) -> Value {
    body[field] = value;
    body
}

/// `forecast`'s line for the item called `name`.
fn line<'a>(forecast: &'a Value, name: &str) -> &'a Value {
    forecast["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|line| line["name"] == name)
        .unwrap()
}

/// The `kind` of each entry of `forecast`'s `field`.
fn kinds(forecast: &Value, field: &str) -> Vec<String> {
    forecast[field]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The entry of `forecast`'s cautions of this `kind`, which must be the only one.
fn caution<'a>(forecast: &'a Value, kind: &str) -> &'a Value {
    let found: Vec<&Value> = forecast["cautions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|caution| caution["kind"] == kind)
        .collect();
    let [only] = found.as_slice() else {
        panic!("expected exactly one caution of the kind");
    };
    only
}

impl World {
    /// A token of alice's with `scope` and `grants`.
    async fn token(&self, scope: Scope, grants: Vec<TokenGrant>) -> SecretToken {
        self.server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap()
            .1
    }

    async fn send_keyed(
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

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
    ) -> (StatusCode, String) {
        self.send_keyed(method, uri, token, body, None).await
    }

    /// Sends `body` as alice's admin token, which must succeed; returns the answer.
    async fn admin_send(&self, method: Method, uri: &str, body: &Value) -> Value {
        let (status, text) = self.send(method, uri, &self.admin, Some(body)).await;
        assert!(status.is_success(), "the setup must succeed: {status}");
        serde_json::from_str(&text).unwrap()
    }

    /// An account, whose body takes `extra` on top of its name, kind and currency.
    pub(crate) async fn account_with(
        &self,
        name: &str,
        kind: &str,
        currency: &str,
        extra: Value,
    ) -> AccountId {
        let mut body = json!({"name": name, "kind": kind, "currency": currency});
        for (field, value) in extra.as_object().unwrap() {
            body[field] = value.clone();
        }
        let answer = self
            .admin_send(Method::POST, "/api/v1/accounts", &body)
            .await;
        serde_json::from_value(answer["id"].clone()).unwrap()
    }

    /// A checking account in USD holding `minor_units` at the end of `BALANCE_ON`.
    pub(crate) async fn funded(&self, name: &str, minor_units: i64) -> AccountId {
        let account = self.account_with(name, "checking", "USD", json!({})).await;
        self.balance(account, BALANCE_ON, minor_units).await;
        account
    }

    pub(crate) async fn balance(&self, account: AccountId, on: &str, minor_units: i64) {
        self.admin_send(
            Method::PUT,
            &format!("/api/v1/accounts/{account}/balances/{on}"),
            &json!({"minor_units": minor_units}),
        )
        .await;
    }

    pub(crate) async fn close(&self, account: AccountId) {
        self.admin_send(
            Method::PATCH,
            &format!("/api/v1/accounts/{account}"),
            &json!({"closed": true}),
        )
        .await;
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

    pub(crate) async fn expense(&self, name: &str) -> CategoryId {
        self.category(name, "expense").await
    }

    /// A stored planned item; returns its id.
    pub(crate) async fn item(&self, body: &Value) -> Value {
        self.admin_send(Method::POST, ITEMS, body).await["id"].clone()
    }

    pub(crate) async fn settings(&self, patch: &Value) {
        self.admin_send(Method::PATCH, SETTINGS, patch).await;
    }

    /// Sets a fixed safety floor of `minor_units` USD.
    pub(crate) async fn floor(&self, minor_units: i64) {
        self.settings(&json!({"floor": {"kind": "fixed", "amount": usd(minor_units)}}))
            .await;
    }

    /// A manual transaction with one split of all of it, linked to `goal` if not null;
    /// returns its id.
    async fn spend_linked(
        &self,
        account: AccountId,
        posted_on: &str,
        minor_units: i64,
        category: CategoryId,
        goal: &Value,
    ) -> Value {
        let id = self.manual(account, posted_on, -minor_units).await;
        self.admin_send(
            Method::PATCH,
            &format!("{TRANSACTIONS}/{}", id.as_str().unwrap()),
            &json!({"splits": [
                {"category": category, "minor_units": -minor_units, "goal": goal}
            ]}),
        )
        .await;
        id
    }

    /// `minor_units` spent from `account` on `category`.
    pub(crate) async fn spend(
        &self,
        account: AccountId,
        posted_on: &str,
        minor_units: i64,
        category: CategoryId,
    ) {
        self.spend_linked(account, posted_on, minor_units, category, &Value::Null)
            .await;
    }

    /// An uncategorized manual transaction, negative for money out; returns its id.
    async fn manual(&self, account: AccountId, posted_on: &str, minor_units: i64) -> Value {
        self.admin_send(
            Method::POST,
            TRANSACTIONS,
            &json!({
                "account": account,
                "posted_on": posted_on,
                "minor_units": minor_units,
                "description": "Purchase",
            }),
        )
        .await["id"]
            .clone()
    }

    /// The forecast with this query string, which must answer 200.
    async fn forecast_at(&self, query: &str) -> Value {
        let (status, text) = self
            .send(
                Method::GET,
                &format!("{FORECAST}{query}"),
                &self.admin,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }

    pub(crate) async fn forecast(&self) -> Value {
        self.forecast_at("").await
    }

    /// The what-if of `body`, which must answer 200.
    async fn what_if(&self, body: &Value) -> Value {
        let (status, text) = self
            .send(Method::POST, WHAT_IF, &self.admin, Some(body))
            .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }
}
