//! The transactions pages through the real router: the list and its filters, the review queue,
//! one transaction's page and its forms, transfer links, possible duplicates, batch review, and
//! transactions entered by hand. Every write is read back through the API and found in the audit
//! log.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions unwrap to fail the test"
)]

mod browse;
mod duplicates;
mod links;
mod manual;
mod review;
mod splits;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName};
use coffer_domain::ids::{AccountId, CategoryId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, Description, Payee, TransactionOrigin};
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::NewManualTransactionInput;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_store::{AuditEvent, AuditFilter, NewTransaction, PageRequest};
use jiff::civil::{Date, date};
use secrecy::ExposeSecret as _;
use serde_json::Value;

const LIST: &str = "/transactions";

struct World {
    server: TestServer,
    /// Alice, an admin, signed in.
    browser: Browser,
    alice: Actor,
    /// A token of alice's that reads what the pages wrote, through the API.
    reader: SecretToken,
    /// An account kept by hand.
    cash: AccountId,
}

async fn world() -> World {
    let (server, browser, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(browser.cookie.as_deref().unwrap())
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
    let cash = account(&server, &alice, "Cash", "USD").await;
    World {
        server,
        browser,
        alice,
        reader,
        cash,
    }
}

async fn account(server: &TestServer, actor: &Actor, name: &str, currency: &str) -> AccountId {
    server
        .ledger
        .create_account(
            actor,
            NewAccountInput {
                name: name.into(),
                kind: AccountKind::Checking,
                currency: Currency::parse(currency).unwrap(),
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

impl World {
    /// A transaction entered by hand in `cash`, as the API would.
    async fn manual(&self, minor_units: i64, payee: &str, on: Date) -> TransactionId {
        self.manual_in(self.cash, minor_units, payee, on).await
    }

    /// A transaction entered by hand in `account`, as the API would.
    async fn manual_in(
        &self,
        account: AccountId,
        minor_units: i64,
        payee: &str,
        on: Date,
    ) -> TransactionId {
        self.server
            .ledger
            .create_manual_transaction(
                &self.alice,
                NewManualTransactionInput {
                    account,
                    posted_on: on,
                    minor_units,
                    description: format!("{payee} purchase"),
                    payee: Some(payee.into()),
                    memo: None,
                    notes: None,
                },
                None,
            )
            .await
            .unwrap()
            .id
    }

    /// An imported transaction in `account`, as a sync would create it.
    async fn imported(&self, account: AccountId, minor_units: i64, payee: &str) -> TransactionId {
        let mut tx = self.server.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Imported,
                account,
                posted_on: date(2026, 10, 3),
                transacted_on: Some(date(2026, 10, 2)),
                pending: false,
                amount_minor: minor_units,
                currency: Currency::parse("USD").unwrap(),
                raw_description: Description::parse("CARD 0042").unwrap(),
                imported_payee: Some(Payee::parse(payee).unwrap()),
                memo: None,
                notes: None,
                created_at: self.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        record.id
    }

    /// An expense category named `name`, in a group of its own.
    async fn category(&self, name: &str) -> CategoryId {
        let group = self
            .server
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
        self.server
            .ledger
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

    /// `GET path` from the API, as the reader token, which must answer `expected`.
    async fn api(&mut self, path: &str, expected: StatusCode) -> Value {
        let request = Request::builder()
            .uri(path)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.reader.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
        let reply = self.browser.send(request).await;
        assert_eq!(reply.status, expected);
        serde_json::from_str(&reply.body).unwrap_or(Value::Null)
    }

    /// The transaction as the API answers it.
    async fn read(&mut self, id: TransactionId) -> Value {
        self.api(&format!("/api/v1/transactions/{id}"), StatusCode::OK)
            .await
    }

    /// A page as alice sees it, which must answer 200.
    async fn page(&mut self, path: &str) -> Reply {
        let reply = self.browser.get(path).await;
        assert_eq!(reply.status, StatusCode::OK);
        reply
    }

    /// Alice's anti-forgery token.
    async fn csrf(&mut self) -> String {
        self.page(LIST).await.csrf()
    }

    /// Posts `fields` and alice's token to `path`.
    async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let csrf = self.csrf().await;
        let mut all = vec![("csrf", csrf.as_str())];
        all.extend_from_slice(fields);
        self.browser.post_form(path, &all).await
    }

    /// The audit events on `id`, newest first.
    async fn audit(&self, id: impl std::fmt::Display) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_id: Some(id.to_string()),
                    ..AuditFilter::default()
                },
                &PageRequest {
                    limit: 50,
                    ..PageRequest::default()
                },
            )
            .await
            .unwrap()
            .items
    }
}

fn detail(id: TransactionId) -> String {
    format!("{LIST}/{id}")
}

/// A 303 to `location`.
fn assert_redirect(reply: &Reply, location: &str) {
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), location);
}

/// A refusal with this status whose page says `words` and doesn't hold `typed`.
fn assert_refused(reply: &Reply, status: StatusCode, words: &str, typed: &str) {
    assert_eq!(reply.status, status);
    assert!(reply.body.contains(words), "says why");
    assert!(!reply.body.contains(typed), "never echoes the input");
}

/// The audit actor of a member signed in.
fn signed_in(actor: &Actor) -> AuditActor {
    AuditActor::Member {
        member: actor.member,
        agent: None,
    }
}

/// A signed-in member "bob", enrolled by invite from alice.
async fn member(world: &World) -> (Browser, Actor) {
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    assert_eq!(
        bob.enroll(&mut key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );
    let actor = world
        .server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap();
    (bob, actor)
}
