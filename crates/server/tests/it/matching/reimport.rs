//! Re-reported records: idempotence, kept user state, held sources, malformed
//! input and manual transactions.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{
    World, account, actions_for, advance, apply, bind, bound_link, category, ignoring_last_seen,
    only_transaction, record, splits, transaction_audit, transactions, unbound_link, window, world,
};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{DisplayName, Role, Scope, TokenName, Username};
use coffer_domain::ids::MemberId;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{MISSING_DESCRIPTION, ReviewStatus, TransactionOrigin};
use coffer_server::http::router;
use coffer_server::ledger::transactions::{NewManualTransactionInput, Patch, TransactionPatch};
use coffer_server::sync::apply::{ApplyReport, Coverage, apply_held};
use coffer_store::NewMember;
use http_body_util::BodyExt as _;
use jiff::civil::date;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

#[tokio::test]
async fn reimport_is_idempotent() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let batch = || {
        vec![
            record("t1", 3, -1_250, "CORNER SHOP 0042", false),
            record("t2", 4, 250_000, "PAYROLL ACME", false),
            record("t3", 5, -499, "SQ *BLUE BOTTLE", true),
        ]
    };
    let first = apply(&world, &link, batch(), &[], window()).await;
    assert_eq!(first.created, 3);
    let before = transactions(&world, checking.id).await;
    let audit_before = transaction_audit(&world).await.len();
    assert_eq!(audit_before, 3, "one transaction.import each");
    advance(&world);

    let second = apply(&world, &link, batch(), &[], window()).await;
    assert_eq!(second, ApplyReport::default(), "nothing changed");
    assert_eq!(
        ignoring_last_seen(transactions(&world, checking.id).await),
        ignoring_last_seen(before)
    );
    assert_eq!(transaction_audit(&world).await.len(), audit_before);
}

#[tokio::test]
async fn user_state_survives_reimport() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let food = category(&world, "Food").await;
    let bob = {
        let mut tx = world.server.store.begin().await.unwrap();
        let bob = tx
            .insert_member(NewMember {
                id: MemberId::new(),
                username: Username::parse("bob").unwrap(),
                display_name: DisplayName::parse("Bob").unwrap(),
                role: Role::Member,
                created_at: world.server.clock.now(),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        bob.id
    };
    let mut original = record("t1", 3, -1_250, "CORNER SHOP 0042", false);
    original.payee = Some("Corner Shop".into());
    apply(&world, &link, vec![original], &[], window()).await;
    let id = only_transaction(&world, checking.id).await.id;
    world
        .ledger
        .update_transaction(
            &world.alice,
            id,
            TransactionPatch {
                payee: Patch::Set("Neighbourhood shop".into()),
                notes: Patch::Set("milk and bread".into()),
                review: Some(ReviewStatus::Reviewed),
                assignee: Patch::Set(bob),
                splits: Some(splits(&[(food, -1_250)])),
            },
        )
        .await
        .unwrap();
    let edited = world.server.store.get_transaction(id).await.unwrap();
    advance(&world);

    let mut changed = record("t1", 4, -1_250, "CORNER SHOP #42 MAIN ST", false);
    changed.payee = Some("Corner Shop Inc".into());
    changed.memo = Some("card 1234".into());
    let report = apply(&world, &link, vec![changed], &[], window()).await;
    assert_eq!(report.updated, 1);

    let after = only_transaction(&world, checking.id).await;
    assert_eq!(after.id, id);
    assert_eq!(after.posted_on, date(2026, 10, 4));
    assert_eq!(after.raw_description.as_str(), "CORNER SHOP #42 MAIN ST");
    assert_eq!(
        after.imported_payee.as_ref().unwrap().as_str(),
        "Corner Shop Inc"
    );
    assert_eq!(after.memo.as_ref().unwrap().as_str(), "card 1234");
    assert_eq!(after.payee, edited.payee, "the edited payee stays");
    assert!(after.payee_edited);
    assert_eq!(after.notes, edited.notes);
    assert_eq!(after.review, ReviewStatus::Reviewed);
    assert_eq!(after.reviewed, edited.reviewed);
    assert_eq!(after.assignee, Some(bob));
    assert_eq!(after.splits, edited.splits);
    assert_eq!(after.categorized, edited.categorized);

    let update = transaction_audit(&world)
        .await
        .into_iter()
        .find(|event| event.action == "transaction.imported_update")
        .unwrap();
    assert_eq!(
        update.actor,
        AuditActor::System {
            reason: "system:sync".into()
        }
    );
    let fields: Vec<&str> = update.changes.iter().map(|(field, _)| field).collect();
    assert!(
        fields.contains(&"description"),
        "description must be among the changed fields"
    );
    assert!(!fields.contains(&"payee"), "the shown payee did not change");
}

#[tokio::test]
async fn manual_transactions_never_matched() {
    let world = world().await;
    let checking = account(&world).await;
    let manual = world
        .ledger
        .create_manual_transaction(
            &world.alice,
            NewManualTransactionInput {
                account: checking.id,
                posted_on: date(2026, 10, 2),
                minor_units: -1_250,
                description: "FARMERS MARKET".into(),
                payee: None,
                memo: None,
                notes: None,
            },
            None,
        )
        .await
        .unwrap();
    let before = world.server.store.get_transaction(manual.id).await.unwrap();
    let link = bound_link(&world, &checking).await;

    let report = apply(
        &world,
        &link,
        vec![record("t1", 2, -1_250, "FARMERS MARKET", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.created, 1);
    let all = transactions(&world, checking.id).await;
    assert_eq!(all.len(), 2);
    let imported = all.iter().find(|t| t.id != manual.id).unwrap();
    assert_eq!(imported.origin, TransactionOrigin::Imported);
    assert_eq!(imported.flags, Vec::new());
    assert_eq!(
        world.server.store.get_transaction(manual.id).await.unwrap(),
        before,
        "the manual transaction is untouched"
    );
}

#[tokio::test]
async fn held_sources_matched_by_apply_held() {
    let world = world().await;
    let checking = account(&world).await;
    // Another provider already reports one of the purchases.
    let other = bound_link(&world, &checking).await;
    apply(
        &world,
        &other,
        vec![record("x-1", 10, -1_250, "CORNER SHOP", false)],
        &[],
        window(),
    )
    .await;
    let existing = only_transaction(&world, checking.id).await;

    let mut link = unbound_link(&world).await;
    let report = apply(
        &world,
        &link,
        vec![
            record("h1", 11, -1_250, "CORNER SHOP 42", false),
            record("h2", 12, -800, "BOOKSHOP", false),
        ],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.held, 2);
    assert_eq!(
        transactions(&world, checking.id).await.len(),
        1,
        "held, not imported"
    );

    bind(&world, &mut link, &checking).await;
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let report = apply_held(
        &mut tx,
        now,
        link.provider_account.clone(),
        checking.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        tx.held_sources(link.provider_account.id).await.unwrap(),
        Vec::new()
    );
    tx.commit().await.unwrap();
    assert_eq!(report.attached, 1, "matched across providers");
    assert_eq!(report.created, 1);

    let all = transactions(&world, checking.id).await;
    assert_eq!(all.len(), 2);
    let shared = all.iter().find(|t| t.id == existing.id).unwrap();
    assert_eq!(shared.sources.len(), 2, "one source per provider");
}

#[tokio::test]
async fn provider_text_is_sanitized_before_insert() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut long = record("t1", 10, -100, &"A".repeat(700), false);
    long.payee = Some(format!("Shop\u{0007}{}", "p".repeat(300)));
    long.memo = Some("line one\nline two".into());
    let mut empty = record("t2", 10, -200, "  \n ", false);
    empty.payee = Some("\t".into());
    empty.memo = Some(String::new());
    apply(&world, &link, vec![long, empty], &[], window()).await;

    let all = transactions(&world, checking.id).await;
    let long = all.iter().find(|t| t.amount_minor == -100).unwrap();
    assert_eq!(long.raw_description.as_str().chars().count(), 500);
    let payee = long.payee.as_ref().unwrap().as_str();
    assert_eq!(payee.chars().count(), 200);
    assert!(payee.starts_with("Shop p"));
    assert_eq!(long.memo.as_ref().unwrap().as_str(), "line one line two");
    assert_eq!(long.sources[0].fields.description, long.raw_description);

    let empty = all.iter().find(|t| t.amount_minor == -200).unwrap();
    assert_eq!(empty.raw_description.as_str(), MISSING_DESCRIPTION);
    assert_eq!(empty.payee, None);
    assert_eq!(empty.memo, None);
}

#[tokio::test]
async fn deleting_a_candidate_cascades() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    let posted = record("posted-1", 11, -1_250, "CORNER SHOP", false);
    apply(&world, &link, vec![pending.clone()], &[], window()).await;
    let pending_id = only_transaction(&world, checking.id).await.id;
    apply(&world, &link, vec![pending, posted.clone()], &[], window()).await;
    let flagged = transactions(&world, checking.id)
        .await
        .into_iter()
        .find(|t| t.id != pending_id)
        .unwrap();
    assert_eq!(flagged.duplicate_candidates, [pending_id]);

    // The pending record goes; its transaction, a candidate, is deleted.
    let report = apply(&world, &link, vec![posted], &["pending-1"], Coverage::Delta).await;
    assert_eq!(report.deleted, 1);
    let left = only_transaction(&world, checking.id).await;
    assert_eq!(left.id, flagged.id);
    assert!(left.duplicate_candidates.is_empty(), "the row went with it");
    assert_eq!(left.flags, Vec::new(), "no candidate is left to review");
    assert_eq!(
        actions_for(&world, left.id)
            .await
            .last()
            .map(String::as_str),
        Some("transaction.candidate_removed")
    );
}

#[tokio::test]
async fn malformed_records_are_skipped() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut no_id = record("", 10, -100, "EMPTY ID", false);
    no_id.provider_id = String::new();
    let long_id = record(&"x".repeat(256), 10, -200, "LONG ID", false);
    let control_id = record("id\u{0000}1", 10, -300, "CONTROL ID", false);
    let mut bad_amount = record("bad-amount", 10, -400, "BAD AMOUNT", false);
    bad_amount.amount_raw = "12,50".into();
    let mut no_amount = record("no-amount", 10, -500, "NO AMOUNT", false);
    no_amount.amount_raw = String::new();
    let mut good = record(&"g".repeat(255), 10, -600, "GOOD", false);
    good.amount_raw = "-6.00".into();
    let report = apply(
        &world,
        &link,
        vec![no_id, long_id, control_id, bad_amount, no_amount, good],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.skipped, 5);
    assert_eq!(report.created, 1, "the account's other records still apply");
    assert_eq!(
        only_transaction(&world, checking.id).await.amount_minor,
        -600
    );
}

/// Sends `body` to `uri` with a fresh write token of alice's.
async fn call(
    world: &World,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .header(header::CONTENT_TYPE, "application/json");
    let request = match body {
        Some(body) => request.body(Body::from(body.to_string())).unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(world.server.app_state())
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

#[tokio::test]
async fn linked_account_refuses_manual_transactions() {
    let world = world().await;
    let checking = account(&world).await;
    let path = format!("/api/v1/accounts/{}", checking.id);
    let (status, body) = call(&world, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "manual");
    let create = json!({"account": checking.id, "posted_on": "2026-10-02",
                        "minor_units": -500, "description": "Coffee"});
    let (status, _) = call(
        &world,
        Method::POST,
        "/api/v1/transactions",
        Some(create.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    bound_link(&world, &checking).await;
    assert!(
        world
            .server
            .store
            .account_is_linked(checking.id)
            .await
            .unwrap()
    );
    let (status, body) = call(&world, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "linked");
    let (status, body) = call(&world, Method::POST, "/api/v1/transactions", Some(create)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflict");

    let (status, body) = call(&world, Method::GET, "/api/v1/transactions", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "only the earlier manual one");
    assert_eq!(items[0]["sources"], json!([]));
    assert_eq!(items[0]["duplicate_candidates"], json!([]));
}

#[tokio::test]
async fn transaction_view_lists_its_sources() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    let mut pending = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    pending.amount_raw = "-12.50".into();
    apply(&world, &link, vec![pending], &[], window()).await;
    let id = only_transaction(&world, checking.id).await.id;

    let view = world
        .ledger
        .get_transaction(&world.alice, id)
        .await
        .unwrap();
    assert_eq!(view.sources.len(), 1);
    let source = &view.sources[0];
    assert_eq!(source.connection, link.connection);
    assert_eq!(source.provider_account, link.provider_account.id);
    assert!(source.pending);
    assert_eq!(source.posted_on, date(2026, 10, 10));
    assert_eq!(source.amount_raw, "-12.50");
    assert_eq!(view.duplicate_candidates, Vec::new());
}

#[tokio::test]
async fn malformed_rereport_of_a_known_record_is_not_vanished() {
    let world = world().await;
    let checking = account(&world).await;
    let link = bound_link(&world, &checking).await;
    apply(
        &world,
        &link,
        vec![record("pending-1", 10, -1_250, "CORNER SHOP", true)],
        &[],
        window(),
    )
    .await;
    let stored = only_transaction(&world, checking.id).await;
    advance(&world);

    // The provider reports the same record again, in a shape we refuse.
    let mut reformatted = record("pending-1", 10, -1_250, "CORNER SHOP", true);
    reformatted.amount_raw = "12,50".into();
    let report = apply(&world, &link, vec![reformatted], &[], window()).await;
    assert_eq!(report.skipped, 1);
    assert_eq!(report.deleted, 0);
    let kept = only_transaction(&world, checking.id).await;
    assert_eq!(kept.id, stored.id);
    assert_eq!(kept.flags, Vec::new());
    assert_eq!(kept.sources.len(), 1);
    assert_eq!(kept.sources[0].provider_txn_id, "pending-1");
}
