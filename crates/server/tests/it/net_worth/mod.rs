//! Net worth history, the FX endpoints and the display currency through the real
//! router (spec §8.10). Rates and the daily fill go through a store handle, standing
//! in for the jobs that do them; everything else goes through the API.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
mod answer;
mod bind;
mod conversion;
mod documented;
mod fx_api;
mod settings;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::fx::{FxErrorCode, FxFetchStatus, FxSource, RateObservation};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::money::Currency;
use coffer_domain::net_worth::DailyValue;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use jiff::Timestamp;
use jiff::civil::Date;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const REPORT: &str = "/api/v1/reports/net-worth";
const SETTINGS: &str = "/api/v1/household/settings";
/// The golden example's day.
const TODAY: &str = "2026-11-02";

pub(crate) struct World {
    server: TestServer,
    /// Alice's token without grants, which may do anything.
    admin: SecretToken,
    admin_id: AgentTokenId,
    alice: Actor,
}

/// Admin "alice" on the day `TODAY` in UTC, with the daily table built and empty.
async fn world() -> World {
    world_on(TestServer::new().await).await
}

/// As [`world`], over `server`, with a table that can answer.
async fn world_on(server: TestServer) -> World {
    let world = unbuilt(server).await;
    world.rebuild(TODAY).await;
    world
}

/// As [`world_on`], with the table as the migration leaves it.
async fn unbuilt(server: TestServer) -> World {
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (record, admin) = server
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
    server.clock.set(noon(TODAY));
    World {
        server,
        admin,
        admin_id: record.id,
        alice,
    }
}

fn noon(day: &str) -> Timestamp {
    format!("{day}T12:00:00Z").parse().unwrap()
}

fn day(text: &str) -> Date {
    text.parse().unwrap()
}

fn code(text: &str) -> Currency {
    Currency::parse(text).unwrap()
}

fn on(holder: AccountId, level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(holder),
        level,
    }
}

fn all(level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::All,
        level,
    }
}

fn money(minor_units: i64, currency: &str) -> Value {
    json!({"minor_units": minor_units, "currency": currency})
}

fn usd(minor_units: i64) -> Value {
    money(minor_units, "USD")
}

/// `report`'s point on `on`.
fn point<'a>(report: &'a Value, on: &str) -> &'a Value {
    let found = report["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["on"] == on);
    found.unwrap_or_else(|| panic!("no point on that day"))
}

/// The days of `report`'s points, oldest first.
fn days(report: &Value) -> Vec<String> {
    report["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["on"].as_str().unwrap().to_owned())
        .collect()
}

/// `report`'s series of `holder`.
fn series(report: &Value, holder: AccountId) -> &Value {
    let wanted = holder.to_string();
    let found = report["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|one| one["account"] == wanted.as_str());
    found.unwrap_or_else(|| panic!("no series for that holder"))
}

/// `holder`'s point on `on` in `report`.
fn holder_point<'a>(report: &'a Value, holder: AccountId, on: &str) -> &'a Value {
    let found = series(report, holder)["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["on"] == on);
    found.unwrap_or_else(|| panic!("no value on that day"))
}

/// The entries of `list` (`blockers` or `cautions`) of this `kind`.
fn of_kind<'a>(report: &'a Value, list: &str, kind: &str) -> Vec<&'a Value> {
    report[list]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == kind)
        .collect()
}

/// The one `rate_missing` caution of `report`, which must be about `currency`.
fn rate_missing<'a>(report: &'a Value, currency: &str) -> &'a Value {
    let found: Vec<&Value> = of_kind(report, "cautions", "rate_missing")
        .into_iter()
        .filter(|caution| caution["currency"] == currency)
        .collect();
    let [only] = found.as_slice() else {
        panic!("expected one caution for that currency");
    };
    only
}

impl World {
    fn at(&self, day: &str) {
        self.server.clock.set(noon(day));
    }

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

    /// A read token that sees only `holder`, at `balances`.
    async fn sees_only(&self, holder: AccountId) -> SecretToken {
        self.token(Scope::Read, vec![on(holder, AccountLevel::Balances)])
            .await
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
    ) -> (StatusCode, String) {
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
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// `body` sent as alice's admin token, which must succeed; the answer.
    async fn admin_send(&self, method: Method, uri: &str, body: &Value) -> Value {
        let (status, text) = self.send(method, uri, &self.admin, Some(body)).await;
        assert!(status.is_success(), "the setup must succeed: {status}");
        serde_json::from_str(&text).unwrap()
    }

    /// The raw answer to `?query` for `token`.
    async fn report_as(&self, token: &SecretToken, query: &str) -> (StatusCode, String) {
        self.send(Method::GET, &format!("{REPORT}?{query}"), token, None)
            .await
    }

    /// The report for `?query` as the household's admin, which must be 200.
    async fn report(&self, query: &str) -> Value {
        let (status, text) = self.report_as(&self.admin, query).await;
        assert_eq!(status, StatusCode::OK, "the report answers");
        serde_json::from_str(&text).unwrap()
    }

    /// An account with `extra` on top of its name, kind and currency.
    async fn holding_with(
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

    /// A checking account in `currency`.
    async fn holding(&self, name: &str, currency: &str) -> AccountId {
        self.holding_with(name, "checking", currency, json!({}))
            .await
    }

    async fn balance(&self, holder: AccountId, on: &str, minor_units: i64) {
        self.admin_send(
            Method::PUT,
            &format!("/api/v1/accounts/{holder}/balances/{on}"),
            &json!({"minor_units": minor_units}),
        )
        .await;
    }

    async fn patch_holding(&self, holder: AccountId, body: &Value) {
        self.admin_send(Method::PATCH, &format!("/api/v1/accounts/{holder}"), body)
            .await;
    }

    async fn display(&self, currency: &str) {
        self.admin_send(
            Method::PATCH,
            SETTINGS,
            &json!({"display_currency": currency}),
        )
        .await;
    }

    /// Stores `rates` of `currency`, as a fetch does.
    async fn rates(&self, currency: &str, rates: &[(&str, &str)]) {
        let rates: Vec<RateObservation> = rates
            .iter()
            .map(|(on, rate)| RateObservation {
                on: day(on),
                rate: rate.parse().unwrap(),
            })
            .collect();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.upsert_fx_rates(
            code(currency),
            FxSource::Ecb,
            &rates,
            self.server.clock.now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// Records that fetching `currency`'s rates gave none, as a fetch does.
    async fn fetch_failed(
        &self,
        currency: &str,
        status: FxFetchStatus,
        error: Option<FxErrorCode>,
    ) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.record_fx_failure(code(currency), status, error, self.server.clock.now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    /// What the daily job does when a day has passed.
    async fn fill(&self, through: &str) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.fill_net_worth(day(through)).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// What the daily job does after the migration.
    async fn rebuild(&self, through: &str) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.rebuild_net_worth(day(through)).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Every row of the daily table.
    async fn rows(&self) -> Vec<DailyValue> {
        self.server
            .store
            .daily_values_for_tests()
            .await
            .unwrap_or_else(|_| panic!("reading the table failed"))
    }

    async fn settings_events(&self) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_type: Some("household".into()),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap_or_else(|_| panic!("reading the log failed"))
            .items
    }

    /// How many entries the whole log holds, up to one page.
    async fn audit_count(&self) -> usize {
        self.server
            .store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap_or_else(|_| panic!("reading the log failed"))
            .items
            .len()
    }

    /// Task 1's household (see `golden_week_of_net_worth`): the rates, the four accounts
    /// and their balances, and display USD.
    async fn golden(&self) -> Golden {
        self.rates(
            "USD",
            &[
                ("2026-10-26", "1.1188"),
                ("2026-10-27", "1.115"),
                ("2026-10-28", "1.1133"),
                ("2026-10-29", "1.113"),
                ("2026-10-30", "1.1172"),
            ],
        )
        .await;
        let checking = self.holding("Checking", "USD").await;
        self.balance(checking, "2026-10-26", 500_000).await;
        self.balance(checking, "2026-10-30", 550_000).await;
        let livret = self
            .holding_with("Livret", "savings", "EUR", json!({"liquidity": "cash"}))
            .await;
        self.balance(livret, "2026-10-26", 100_000).await;
        let card = self
            .holding_with(
                "Card",
                "credit_card",
                "USD",
                json!({"liquidity": "short_term_liability"}),
            )
            .await;
        self.balance(card, "2026-10-28", -30_000).await;
        let side = self
            .holding_with(
                "Side",
                "checking",
                "USD",
                json!({"include_in_net_worth": false}),
            )
            .await;
        self.balance(side, "2026-10-26", 99_900).await;
        self.display("USD").await;
        Golden {
            checking,
            livret,
            card,
            side,
        }
    }
}

/// The accounts of [`World::golden`].
struct Golden {
    checking: AccountId,
    livret: AccountId,
    card: AccountId,
    side: AccountId,
}
