//! The `/accounts` pages: every account grouped with what it is, one account's page with its
//! balances, and the forms that create, change and close an account and record a balance, all
//! through the services the API uses, read back through the API.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod balances;
mod change;
mod create;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName};
use coffer_domain::ids::AccountId;
use coffer_domain::money::Currency;
use coffer_server::ledger::accounts::{NewAccountInput, OpeningBalance};
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::Value;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                   frame-ancestors 'none'; form-action 'self'; base-uri 'none'; report-uri /browser-reports";
const CHILD: &str = "Child A";

struct World {
    server: TestServer,
    admin: Browser,
    alice: Actor,
    /// A read token, to read what the pages stored back through the API.
    reader: SecretToken,
}

async fn world() -> World {
    let (server, admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let (_, reader) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    World {
        server,
        admin,
        alice,
        reader,
    }
}

/// A manual account with nothing chosen but its name, kind and currency.
fn input(name: &str, kind: AccountKind, currency: &str) -> NewAccountInput {
    NewAccountInput {
        name: name.to_owned(),
        kind,
        currency: Currency::parse(currency).unwrap(),
        liquidity: None,
        include_in_net_worth: None,
        include_in_forecast: None,
        belongs_to: None,
        beneficiary: None,
        opening_balance: None,
    }
}

fn opening(on: &str, minor_units: i64) -> OpeningBalance {
    OpeningBalance {
        on: on.parse().unwrap(),
        minor_units,
    }
}

impl World {
    /// Creates an account through the service, as alice.
    async fn holding(&self, input: NewAccountInput) -> AccountId {
        self.server
            .ledger
            .create_account(&self.alice, input, None)
            .await
            .unwrap()
            .id
    }

    /// `GET path` on the API with the read token.
    async fn api(&mut self, path: &str) -> Value {
        let request = self
            .admin
            .builder(Method::GET, path)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.reader.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
        let reply = self.admin.send(request).await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.json()
    }

    async fn account(&mut self, id: &str) -> Value {
        self.api(&format!("/api/v1/accounts/{id}")).await
    }

    async fn every_account(&mut self) -> Vec<Value> {
        self.api("/api/v1/accounts?include_closed=true").await["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn audited(&self, id: &str, action: &str) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_type: Some("account".into()),
                    entity_id: Some(id.to_owned()),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }

    /// A member "bob", signed in.
    async fn member(&self) -> Browser {
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
        bob
    }
}

/// Posts `fields` and the anti-forgery token, taken from `/accounts`, to `path`.
async fn post(browser: &mut Browser, path: &str, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/accounts").await.csrf();
    let mut sent = vec![("csrf", csrf.as_str())];
    sent.extend_from_slice(fields);
    browser.post_form(path, &sent).await
}

fn by_member(event: &AuditEvent, actor: &Actor) -> bool {
    matches!(event.actor, AuditActor::Member { member, agent: None } if member == actor.member)
}

fn assert_inline_free(reply: &Reply) {
    assert_eq!(reply.header(header::CONTENT_SECURITY_POLICY), CSP);
    assert!(!reply.body.contains("style="));
    assert!(!reply.body.contains("<style"));
    assert!(!reply.body.contains("<script>"));
}

/// The text of the page from `start` to the next `end`.
fn section<'a>(body: &'a str, start: &str, end: &str) -> &'a str {
    let from = body.find(start).unwrap();
    let rest = &body[from..];
    &rest[..rest.find(end).unwrap_or(rest.len())]
}

#[tokio::test]
async fn an_empty_list_offers_the_form() {
    let mut world = world().await;
    let page = world.admin.get("/accounts").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    assert!(page.body.contains("aria-current=\"page\">More<"));
    assert!(page.body.contains("illustration-empty"));
    assert!(page.body.contains("New account kept by hand"));
}

#[tokio::test]
async fn accounts_are_grouped_with_what_they_are() {
    let mut world = world().await;
    let mut checking = input("Everyday", AccountKind::Checking, "USD");
    checking.opening_balance = Some(opening("2026-01-02", 123_456));
    world.holding(checking).await;
    let mut brokerage = input("Shares", AccountKind::Brokerage, "USD");
    brokerage.include_in_net_worth = Some(false);
    world.holding(brokerage).await;
    let mut plan = input("Plan", AccountKind::Education, "USD");
    plan.beneficiary = Some(CHILD.to_owned());
    world.holding(plan).await;
    let old = world
        .holding(input("Old card", AccountKind::CreditCard, "EUR"))
        .await;
    world
        .server
        .ledger
        .update_account(
            &world.alice,
            old,
            coffer_server::ledger::accounts::AccountPatch {
                closed: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let page = world.admin.get("/accounts").await;
    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    let household = section(&page.body, "<h2>The household's</h2>", "</section>");
    let cash = section(household, "<h3>Cash</h3>", "</ul>");
    assert!(cash.contains(">Everyday</a>"));
    assert!(cash.contains("Checking · USD · Kept by hand"));
    assert!(cash.contains("1,234.56 USD"));
    assert!(cash.contains("on 2 January 2026"));
    assert!(cash.contains("Counts as cash in the forecast"));
    let invested = section(household, "<h3>Invested</h3>", "</ul>");
    assert!(invested.contains(">Shares</a>"));
    assert!(invested.contains("Left out of net worth"));
    assert!(invested.contains("Not in the forecast, which counts only cash and debts due soon"));
    assert!(invested.contains("No balance yet"));

    let not_ours = section(&page.body, "<h2>Not the household's</h2>", "</section>");
    assert!(not_ours.contains("buckets beside the total"));
    assert!(not_ours.contains(">Plan</a>"));
    assert!(not_ours.contains("Held for Child A"));
    assert!(not_ours.contains("Not in the forecast, since it isn't the household's"));

    let closed = section(&page.body, "Closed (1)", "</section>");
    assert!(closed.contains(">Old card</a>"));
    assert!(closed.contains("Not in the forecast, since it is closed"));
    assert!(!household.contains("Old card"));
}

#[tokio::test]
async fn an_account_s_page_says_what_it_is_and_pages_its_balances() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let first: jiff::civil::Date = "2026-01-01".parse().unwrap();
    for offset in 0..55 {
        let on = first.checked_add(jiff::Span::new().days(offset)).unwrap();
        world
            .server
            .ledger
            .set_manual_balance(&world.alice, id, on, 100 * offset)
            .await
            .unwrap();
    }
    let path = format!("/accounts/{id}");

    let page = world.admin.get(&path).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    assert!(page.body.contains("<h1>Everyday</h1>"));
    assert!(page.body.contains("aria-current=\"page\">More<"));
    assert!(page.body.contains("Counts as cash in the forecast"));
    let history = section(&page.body, "id=\"balances\"", "</section>");
    assert!(history.contains(">24 February 2026<"), "newest first");
    assert!(history.contains("54.00 USD"));
    assert!(history.contains("Entered by hand"));
    assert!(!history.contains(">1 January 2026<"));
    let older = page.between("<a href=\"/accounts/", "\">Older balances");
    let older = format!("/accounts/{}", older.replace("&amp;", "&"));

    let next = world.admin.get(&older).await;
    assert_eq!(next.status, StatusCode::OK);
    let history = section(&next.body, "id=\"balances\"", "</section>");
    assert!(history.contains(">1 January 2026<"));
    assert!(!history.contains(">24 February 2026<"));
    assert!(!history.contains("Older balances"));
    assert!(history.contains("Back to the newest balances"));
}

#[tokio::test]
async fn an_unknown_account_or_address_is_refused() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    assert_eq!(
        world.admin.get("/accounts/not-an-id").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .admin
            .get(&format!("/accounts/{}", AccountId::new()))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let odd = world.admin.get(&format!("/accounts/{id}?page=2")).await;
    assert_eq!(odd.status, StatusCode::UNPROCESSABLE_ENTITY);
    let cursor = world
        .admin
        .get(&format!("/accounts/{id}?after=MARKERZq"))
        .await;
    assert_eq!(cursor.status, StatusCode::BAD_REQUEST);
    assert!(!cursor.body.contains("MARKERZq"));
}

#[tokio::test]
async fn a_member_sees_and_does_what_the_api_lets_them() {
    let world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let mut bob = world.member().await;
    let list = bob.get("/accounts").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains(">Everyday</a>"));
    assert!(list.body.contains("New account kept by hand"));
    let page = bob.get(&format!("/accounts/{id}")).await;
    for form in [
        "Record a balance",
        "Change this account",
        "Close this account",
    ] {
        assert!(page.body.contains(form), "{form}");
    }
}

#[tokio::test]
async fn the_net_worth_page_links_each_account() {
    let world = crate::net_worth::world().await;
    let id = world.holding("Everyday", "EUR").await;
    world.balance(id, "2026-10-02", 100).await;
    world.display("EUR").await;
    let session = world
        .server
        .sessions
        .create(world.alice.member)
        .await
        .unwrap();
    let mut alice = Browser::new(&world.server);
    alice.cookie = Some(session.expose_secret().to_owned());
    let page = alice.get("/net-worth?range=all").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains(&format!("<a href=\"/accounts/{id}\">Everyday</a>"))
    );
}
