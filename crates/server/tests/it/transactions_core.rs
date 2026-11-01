//! The transaction model at the service level: splits and who categorized,
//! review and who reviewed, payee, notes, assignee, and which transactions
//! may be created or deleted.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse as _;
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, DisplayName, Role, Scope, TokenName, Username, Via};
use coffer_domain::ids::{AccountId, CategoryId, MemberId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{
    CategoryKind, Description, Payee, ReviewStatus, SplitInput, TransactionOrigin, UserState,
};
use coffer_server::http::problem::Problem;
use coffer_server::ledger::accounts::{AccountPatch, NewAccountInput};
use coffer_server::ledger::categories::{
    CategoryGroupInput, CategoryGroupPatch, CategoryInput, CategoryPatch,
};
use coffer_server::ledger::transactions::{
    AttributionView, NewManualTransactionInput, Patch, TransactionPatch, TransactionView,
};
use coffer_server::ledger::{Ledger, LedgerError};
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, NewMember, NewTransaction, PageRequest};
use jiff::civil::date;

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

#[tokio::test]
async fn categorize_records_attribution_with_agent() {
    let world = world().await;
    let claude = agent(&world, Scope::Write).await;
    let Via::Agent { token, .. } = claude.via else {
        unreachable!("an agent actor")
    };
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    let created = manual_transaction(&world, account, -1_000).await;
    assert!(created.splits.is_empty(), "uncategorized");
    assert_eq!(created.categorized_by, None);
    advance(&world);

    let view = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -600), (home, -400)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    let parts: Vec<_> = view
        .splits
        .iter()
        .map(|split| (split.category, split.amount.minor_units))
        .collect();
    assert_eq!(parts, [(food, -600), (home, -400)]);
    assert!(
        view.splits
            .iter()
            .all(|split| split.amount.currency.as_str() == "USD")
    );
    let now = world.server.clock.now();
    assert_eq!(
        view.categorized_by,
        Some(AttributionView {
            member: Some(world.alice.member),
            agent: Some(token),
            system: None,
            at: now,
        })
    );
    assert_eq!(view.reviewed_by, None, "categorizing is not reviewing");
    assert_eq!(view.updated_at, now);
    assert_eq!(
        world
            .ledger
            .get_transaction(&claude, created.id)
            .await
            .unwrap(),
        view
    );

    let events = transaction_audit(&world, created.id).await;
    let update = events
        .iter()
        .find(|event| event.action == "transaction.update")
        .unwrap();
    assert_eq!(
        update.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(token),
        }
    );
    let fields: Vec<_> = update.changes.iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["splits"], "only the changed field");
    let change = update.changes.iter().next().unwrap().1;
    assert_eq!(change.before, AuditValue::from("[]"));
    assert_eq!(
        change.after,
        AuditValue::from(format!(
            "[{{\"category\":\"{food}\",\"minor_units\":-600}},\
             {{\"category\":\"{home}\",\"minor_units\":-400}}]"
        ))
    );
}

#[tokio::test]
async fn splits_are_validated_and_need_write() {
    let world = world().await;
    let claude = agent(&world, Scope::Write).await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let home = category(&world, "Home").await;
    let created = manual_transaction(&world, account, -1_000).await;
    world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -1_000)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    for (bad, why) in [
        (
            splits(&[(food, -600), (home, -399)]),
            "the sum is off by one",
        ),
        (splits(&[(food, -1_000), (home, 0)]), "a zero split"),
        (
            splits(&[(CategoryId::new(), -1_000)]),
            "an unknown category",
        ),
    ] {
        let err = world
            .ledger
            .update_transaction(
                &claude,
                created.id,
                TransactionPatch {
                    splits: Some(bad),
                    ..TransactionPatch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{why}: {err:?}");
        assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    }

    // Uncategorizing is a categorization too.
    let cleared = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(Vec::new()),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.splits, []);
    assert_eq!(
        cleared.categorized_by.map(|by| (by.member, by.agent)),
        Some((Some(world.alice.member), None))
    );

    // A read token sees transactions but never changes them.
    let reader = agent(&world, Scope::Read).await;
    assert!(
        world
            .ledger
            .get_transaction(&reader, created.id)
            .await
            .is_ok()
    );
    let err = world
        .ledger
        .update_transaction(
            &reader,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -1_000)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn review_records_reviewer() {
    let world = world().await;
    let account = manual_account(&world).await;
    let created = manual_transaction(&world, account, -250).await;
    assert_eq!(created.review, ReviewStatus::Unreviewed);
    advance(&world);

    let reviewed = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    let reviewed_at = world.server.clock.now();
    assert_eq!(reviewed.review, ReviewStatus::Reviewed);
    assert_eq!(
        reviewed.reviewed_by,
        Some(AttributionView {
            member: Some(world.alice.member),
            agent: None,
            system: None,
            at: reviewed_at,
        })
    );
    assert_eq!(reviewed.categorized_by, None);

    // Reviewing again keeps the first reviewer.
    advance(&world);
    let claude = agent(&world, Scope::Write).await;
    let again = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(again.reviewed_by.map(|by| by.at), Some(reviewed_at));

    let unreviewed = world
        .ledger
        .update_transaction(
            &claude,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Unreviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unreviewed.review, ReviewStatus::Unreviewed);
    assert_eq!(unreviewed.reviewed_by, None);

    let events = transaction_audit(&world, created.id).await;
    let reviews: Vec<_> = events
        .iter()
        .filter(|event| event.action == "transaction.update")
        .map(|event| {
            let (field, change) = event.changes.iter().next().unwrap();
            (
                field.to_owned(),
                change.before.clone(),
                change.after.clone(),
            )
        })
        .collect();
    assert_eq!(reviews.len(), 2, "the repeated review wrote nothing");
    assert!(reviews.contains(&(
        "review".into(),
        AuditValue::from("unreviewed"),
        AuditValue::from("reviewed")
    )));
    assert!(reviews.contains(&(
        "review".into(),
        AuditValue::from("reviewed"),
        AuditValue::from("unreviewed")
    )));
}

#[tokio::test]
async fn payee_clear_restores_imported() {
    let world = world().await;
    let account = manual_account(&world).await;
    let id = imported_transaction(&world, account, -1_299).await;
    let seeded = world
        .ledger
        .get_transaction(&world.alice, id)
        .await
        .unwrap();
    assert_eq!(seeded.payee.as_deref(), Some("Corner shop"));
    assert_eq!(seeded.description, "CORNER SHOP 0042");
    assert_eq!(seeded.origin, TransactionOrigin::Imported);
    assert_eq!(seeded.transacted_on, Some(date(2026, 10, 2)));

    let edited = world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Set("  Grocer ".into()),
                notes: Patch::Set("milk\nbread".into()),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.payee.as_deref(), Some("Grocer"));
    assert_eq!(edited.notes.as_deref(), Some("milk\nbread"));
    assert!(
        world
            .server
            .store
            .get_transaction(id)
            .await
            .unwrap()
            .payee_edited
    );

    let restored = world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Clear,
                notes: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(restored.payee.as_deref(), Some("Corner shop"));
    assert_eq!(restored.notes, None);
    let stored = world.server.store.get_transaction(id).await.unwrap();
    assert!(!stored.payee_edited);
    assert_eq!(stored.user_state(), UserState::default());

    for (patch, why) in [
        (
            TransactionPatch {
                payee: Patch::Set("   ".into()),
                ..TransactionPatch::default()
            },
            "a blank payee",
        ),
        (
            TransactionPatch {
                payee: Patch::Set("x".repeat(201)),
                ..TransactionPatch::default()
            },
            "a payee over 200 characters",
        ),
        (
            TransactionPatch {
                notes: Patch::Set("tab\there".into()),
                ..TransactionPatch::default()
            },
            "a control character in notes",
        ),
    ] {
        let err = world
            .ledger
            .update_transaction(&world.alice, id, patch)
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{why}: {err:?}");
    }

    // A manual transaction has no imported payee: clearing leaves none.
    let manual = manual_transaction(&world, account, -100).await;
    assert_eq!(manual.payee.as_deref(), Some("Market stall"));
    let cleared = world
        .ledger
        .update_transaction(
            &world.alice,
            manual.id,
            TransactionPatch {
                payee: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.payee, None);
}

#[tokio::test]
async fn archived_category_rejected() {
    let world = world().await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let travel = category(&world, "Travel").await;
    let created = manual_transaction(&world, account, -500).await;
    let categorized = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -500)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();

    let archived = world
        .ledger
        .update_category(
            &world.alice,
            food,
            CategoryPatch {
                archived: Some(true),
                ..CategoryPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(archived.archived);
    let err = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(food, -200), (travel, -300)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");
    assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        world
            .ledger
            .get_transaction(&world.alice, created.id)
            .await
            .unwrap(),
        categorized,
        "unchanged"
    );

    // A category in an archived group is archived too.
    let tree = world.ledger.list_categories(&world.alice).await.unwrap();
    let travel_group = tree
        .items
        .iter()
        .find(|group| group.categories.iter().any(|c| c.id == travel))
        .unwrap()
        .id;
    world
        .ledger
        .update_category_group(
            &world.alice,
            travel_group,
            CategoryGroupPatch {
                archived: Some(true),
                ..CategoryGroupPatch::default()
            },
        )
        .await
        .unwrap();
    let err = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                splits: Some(splits(&[(travel, -500)])),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");

    // Splits already on an archived category stay, and a patch that
    // repeats them changes nothing.
    let same = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                review: Some(ReviewStatus::Reviewed),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(same.splits, categorized.splits);
}

#[tokio::test]
async fn assignee_must_be_active_member() {
    let world = world().await;
    let account = manual_account(&world).await;
    let created = manual_transaction(&world, account, -800).await;
    let bob = member(&world, "bob").await;
    let carol = member(&world, "carol").await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_member_disabled(carol, world.server.clock.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    for candidate in [carol, MemberId::new()] {
        let err = world
            .ledger
            .update_transaction(
                &world.alice,
                created.id,
                TransactionPatch {
                    assignee: Patch::Set(candidate),
                    ..TransactionPatch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::Invalid(_)), "{err:?}");
        assert_eq!(status(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let assigned = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                assignee: Patch::Set(bob),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(assigned.assignee, Some(bob));
    let unassigned = world
        .ledger
        .update_transaction(
            &world.alice,
            created.id,
            TransactionPatch {
                assignee: Patch::Clear,
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unassigned.assignee, None);
}

#[tokio::test]
async fn manual_transaction_refused_on_closed_account() {
    let world = world().await;
    let account = manual_account(&world).await;
    world
        .ledger
        .update_account(
            &world.alice,
            account,
            AccountPatch {
                closed: Some(true),
                ..AccountPatch::default()
            },
        )
        .await
        .unwrap();
    let input = NewManualTransactionInput {
        account,
        posted_on: date(2026, 10, 2),
        minor_units: -100,
        description: "Coffee".into(),
        payee: None,
        memo: None,
        notes: None,
    };
    let err = world
        .ledger
        .create_manual_transaction(&world.alice, input.clone(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Conflict(..)), "{err:?}");
    assert_eq!(status(err), StatusCode::CONFLICT);

    let err = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: AccountId::new(),
                ..input.clone()
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::NOT_FOUND);

    let reader = agent(&world, Scope::Read).await;
    let err = world
        .ledger
        .create_manual_transaction(&reader, input.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);

    // Once reopened it takes transactions again, in its currency.
    world
        .ledger
        .update_account(
            &world.alice,
            account,
            AccountPatch {
                closed: Some(false),
                ..AccountPatch::default()
            },
        )
        .await
        .unwrap();
    let created = world
        .ledger
        .create_manual_transaction(&world.alice, input, None)
        .await
        .unwrap();
    assert_eq!(created.amount.minor_units, -100);
    assert_eq!(created.amount.currency.as_str(), "USD");
    assert_eq!(created.origin, TransactionOrigin::Manual);
    assert!(!created.pending);
    let events = transaction_audit(&world, created.id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "transaction.create");
}

#[tokio::test]
async fn delete_imported_refused() {
    let world = world().await;
    let account = manual_account(&world).await;
    let imported = imported_transaction(&world, account, -400).await;
    let err = world
        .ledger
        .delete_transaction(&world.alice, imported)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Conflict(..)), "{err:?}");
    assert_eq!(status(err), StatusCode::CONFLICT);
    assert!(world.server.store.get_transaction(imported).await.is_ok());

    let manual = manual_transaction(&world, account, -400).await;
    let reader = agent(&world, Scope::Read).await;
    let err = world
        .ledger
        .delete_transaction(&reader, manual.id)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::FORBIDDEN);
    world
        .ledger
        .delete_transaction(&world.alice, manual.id)
        .await
        .unwrap();
    let err = world
        .ledger
        .get_transaction(&world.alice, manual.id)
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::NotFound));
    let actions: Vec<_> = transaction_audit(&world, manual.id)
        .await
        .into_iter()
        .map(|event| event.action)
        .collect();
    assert!(
        actions.contains(&"transaction.delete".to_owned()),
        "the audit log lacks the delete action"
    );
    let err = world
        .ledger
        .delete_transaction(&world.alice, manual.id)
        .await
        .unwrap_err();
    assert_eq!(status(err), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn no_op_patch_not_audited() {
    let world = world().await;
    let account = manual_account(&world).await;
    let food = category(&world, "Food").await;
    let bob = member(&world, "bob").await;
    let created = manual_transaction(&world, account, -700).await;
    let full = TransactionPatch {
        payee: Patch::Set("Grocer".into()),
        notes: Patch::Set("weekly".into()),
        review: Some(ReviewStatus::Reviewed),
        assignee: Patch::Set(bob),
        splits: Some(splits(&[(food, -700)])),
    };
    let first = world
        .ledger
        .update_transaction(&world.alice, created.id, full.clone())
        .await
        .unwrap();
    let audited = transaction_audit(&world, created.id).await.len();
    advance(&world);

    for patch in [full, TransactionPatch::default()] {
        let again = world
            .ledger
            .update_transaction(&world.alice, created.id, patch)
            .await
            .unwrap();
        assert_eq!(again, first, "nothing changed, not even updated_at");
    }
    assert_eq!(transaction_audit(&world, created.id).await.len(), audited);

    // Clearing what is already clear changes nothing either.
    let id = imported_transaction(&world, account, -50).await;
    let before = transaction_audit(&world, id).await.len();
    world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Clear,
                notes: Patch::Clear,
                assignee: Patch::Clear,
                review: Some(ReviewStatus::Unreviewed),
                splits: Some(Vec::new()),
            },
        )
        .await
        .unwrap();
    assert_eq!(transaction_audit(&world, id).await.len(), before);
}
