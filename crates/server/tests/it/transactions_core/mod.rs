//! The transaction model at the service level: splits and who categorized,
//! review and who reviewed, payee, notes, assignee, and which transactions
//! may be created or deleted.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod categorization;
mod fields;
mod lifecycle;

use axum::http::StatusCode;
use axum::response::IntoResponse as _;
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::{Actor, DisplayName, Role, Scope, TokenName, Username, Via};
use coffer_domain::ids::{AccountId, CategoryId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{
    CategoryKind, Description, Payee, SplitInput, TransactionOrigin,
};
use coffer_server::http::problem::Problem;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::ledger::categories::{CategoryGroupInput, CategoryInput};
use coffer_server::ledger::transactions::{NewManualTransactionInput, TransactionView};
use coffer_server::ledger::{Ledger, LedgerError};
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, NewMember, NewTransaction, PageRequest};
use jiff::civil::date;
use std::sync::Arc;

struct World {
    server: TestServer,
    ledger: Arc<Ledger>,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let ledger = server.app_state().ledger;
    World {
        server,
        ledger,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

/// Alice acting through a new agent token of `scope`.
async fn agent(world: &World, scope: Scope) -> Actor {
    let (record, _) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            scope,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    Actor {
        via: Via::Agent {
            token: record.id,
            scope,
        },
        ..world.alice
    }
}

async fn member(world: &World, username: &str) -> MemberId {
    let mut tx = world.server.store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse(username).unwrap(),
            display_name: DisplayName::parse(username).unwrap(),
            role: Role::Member,
            created_at: world.server.clock.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    member.id
}

async fn manual_account(world: &World) -> AccountId {
    world
        .ledger
        .create_account(
            &world.alice,
            NewAccountInput {
                name: "Checking".into(),
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

async fn category(world: &World, name: &str) -> CategoryId {
    let group = world
        .ledger
        .create_category_group(
            &world.alice,
            CategoryGroupInput {
                name: format!("{name} group"),
                position: None,
            },
            None,
        )
        .await
        .unwrap();
    world
        .ledger
        .create_category(
            &world.alice,
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

async fn manual_transaction(
    world: &World,
    account: AccountId,
    minor_units: i64,
) -> TransactionView {
    world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account,
                posted_on: date(2026, 10, 2),
                minor_units,
                description: "Farmers market".into(),
                payee: Some("Market stall".into()),
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap()
}

/// An imported transaction, as a sync would create it.
async fn imported_transaction(
    world: &World,
    account: AccountId,
    minor_units: i64,
) -> TransactionId {
    let mut tx = world.server.store.begin().await.unwrap();
    let record = tx
        .insert_transaction(NewTransaction {
            origin: TransactionOrigin::Imported,
            account,
            posted_on: date(2026, 10, 3),
            transacted_on: Some(date(2026, 10, 2)),
            pending: false,
            amount_minor: minor_units,
            currency: Currency::parse("USD").unwrap(),
            raw_description: Description::parse("CORNER SHOP 0042").unwrap(),
            imported_payee: Some(Payee::parse("Corner shop").unwrap()),
            memo: None,
            notes: None,
            created_at: world.server.clock.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    record.id
}

fn splits(parts: &[(CategoryId, i64)]) -> Vec<SplitInput> {
    parts
        .iter()
        .map(|(category, minor_units)| SplitInput {
            category: *category,
            minor_units: *minor_units,
            goal: None,
        })
        .collect()
}

async fn transaction_audit(world: &World, id: TransactionId) -> Vec<AuditEvent> {
    world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("transaction".into()),
                entity_id: Some(id.to_string()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items
}

fn status(err: LedgerError) -> StatusCode {
    Problem::from(err).into_response().status()
}

fn advance(world: &World) {
    world
        .server
        .clock
        .advance(jiff::SignedDuration::from_secs(60));
}
