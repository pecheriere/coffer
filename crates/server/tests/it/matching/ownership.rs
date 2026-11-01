//! One purchase reported by several connections: which record owns the imported
//! fields.

use super::{
    Link, World, account, advance, apply, bound_link, ignoring_last_seen, only_transaction, record,
    transaction_audit, window, world,
};
use coffer_domain::ids::TransactionId;
use coffer_domain::time::Clock as _;
use coffer_server::sync::apply::{ApplyReport, Coverage};
use coffer_store::{AccountRecord, AuditEvent, ConnectionStatus, ConnectionStatusChange};

#[tokio::test]
async fn same_purchase_from_two_providers_is_one_transaction() {
    let world = world().await;
    let checking = account(&world).await;
    let first = bound_link(&world, &checking).await;
    let second = bound_link(&world, &checking).await;
    let purchase = || vec![record("p-1", 10, -2_999, "HARDWARE STORE 12", false)];
    apply(&world, &first, purchase(), &[], window()).await;
    let report = apply(&world, &second, purchase(), &[], window()).await;
    assert_eq!(report.attached, 1);
    let shared = only_transaction(&world, checking.id).await;
    assert_eq!(shared.sources.len(), 2);
    assert_ne!(shared.sources[0].connection, shared.sources[1].connection);
    assert_eq!(shared.flags, Vec::new());
    let audit_count = transaction_audit(&world).await.len();
    advance(&world);

    assert_eq!(
        apply(&world, &first, purchase(), &[], window()).await,
        ApplyReport::default()
    );
    assert_eq!(
        apply(&world, &second, purchase(), &[], window()).await,
        ApplyReport::default()
    );
    assert_eq!(
        ignoring_last_seen(vec![only_transaction(&world, checking.id).await]),
        ignoring_last_seen(vec![shared])
    );
    assert_eq!(transaction_audit(&world).await.len(), audit_count);
}

/// Imported-field ownership across connections (spec §8.4): the two links
/// report one purchase, under different descriptions.
async fn shared_purchase(world: &World) -> (Link, Link, AccountRecord) {
    let account = account(world).await;
    let first = bound_link(world, &account).await;
    let second = bound_link(world, &account).await;
    (first, second, account)
}

fn imported_updates(audit: &[AuditEvent], id: TransactionId) -> usize {
    audit
        .iter()
        .filter(|event| {
            event.entity_id == id.to_string() && event.action == "transaction.imported_update"
        })
        .count()
}

/// The connection that reported a purchase first owns its imported fields:
/// the second one attaches without rewriting them, and alternating syncs
/// write nothing, whatever the other connection reports.
#[tokio::test]
async fn second_connection_attaches_without_rewriting_imported_fields() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME STORE", false)],
        &[],
        window(),
    )
    .await;
    advance(&world);
    let report = apply(
        &world,
        &second,
        vec![record("b", 10, -500, "ACME STORE PURCHASE 77", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.attached, 1, "one purchase, two records");
    let settled = only_transaction(&world, account.id).await;
    assert_eq!(settled.sources.len(), 2);
    assert_eq!(settled.raw_description.as_str(), "ACME STORE");

    for round in 0..3 {
        advance(&world);
        apply(
            &world,
            &first,
            vec![record("a", 10, -500, "ACME STORE", false)],
            &[],
            window(),
        )
        .await;
        advance(&world);
        // Even a record that changes each time: it does not own the fields.
        let description = format!("ACME STORE PURCHASE {round}");
        let report = apply(
            &world,
            &second,
            vec![record("b", 10, -500, &description, false)],
            &[],
            window(),
        )
        .await;
        assert_eq!(report.updated, 0, "the other connection changes nothing");
    }

    let after = only_transaction(&world, account.id).await;
    assert_eq!(after.raw_description.as_str(), "ACME STORE");
    assert_eq!(
        after.updated_at, settled.updated_at,
        "nothing was rewritten"
    );
    let audit = transaction_audit(&world).await;
    assert_eq!(imported_updates(&audit, after.id), 0, "nothing was audited");
    let stored_other = after
        .sources
        .iter()
        .find(|source| source.provider_txn_id == "b")
        .unwrap();
    assert_eq!(
        stored_other.fields.description.as_str(),
        "ACME STORE PURCHASE 2",
        "the other record is still kept up to date"
    );
}

/// The owning connection's own corrections still apply, once.
#[tokio::test]
async fn owning_connection_still_updates_imported_fields() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME STORE", false)],
        &[],
        window(),
    )
    .await;
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "OTHER TEXT", false)],
        &[],
        window(),
    )
    .await;

    advance(&world);
    let corrected = vec![record("a", 10, -650, "ACME STORE CORRECTED", false)];
    let report = apply(&world, &first, corrected.clone(), &[], window()).await;
    assert_eq!(report.updated, 1);

    advance(&world);
    let report = apply(&world, &first, corrected, &[], window()).await;
    assert_eq!(report.updated, 0);
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "OTHER TEXT", false)],
        &[],
        window(),
    )
    .await;

    let after = only_transaction(&world, account.id).await;
    assert_eq!(after.raw_description.as_str(), "ACME STORE CORRECTED");
    assert_eq!(after.amount_minor, -650);
    let audit = transaction_audit(&world).await;
    assert_eq!(imported_updates(&audit, after.id), 1, "written once");
}

/// A posted record outranks a pending one: the transaction is not left
/// pending while one connection says it posted. Each change of hands takes
/// one write, and re-reports none.
#[tokio::test]
async fn posted_record_outranks_a_pending_owner() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME PENDING", true)],
        &[],
        window(),
    )
    .await;
    advance(&world);
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "ACME POSTED", false)],
        &[],
        window(),
    )
    .await;

    let posted = only_transaction(&world, account.id).await;
    assert!(!posted.pending, "the posted record decides");
    assert_eq!(posted.raw_description.as_str(), "ACME POSTED");

    // Re-reports of the pending record write nothing.
    advance(&world);
    let report = apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME PENDING", true)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.updated, 0);

    // The first connection posts too; it saw the purchase first, so it owns
    // the fields again, once.
    advance(&world);
    let report = apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME FINAL", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.updated, 1);
    let settled = only_transaction(&world, account.id).await;
    assert_eq!(settled.raw_description.as_str(), "ACME FINAL");

    for _ in 0..2 {
        advance(&world);
        apply(
            &world,
            &second,
            vec![record("b", 10, -500, "ACME POSTED", false)],
            &[],
            window(),
        )
        .await;
        apply(
            &world,
            &first,
            vec![record("a", 10, -500, "ACME FINAL", false)],
            &[],
            window(),
        )
        .await;
    }
    let after = only_transaction(&world, account.id).await;
    assert_eq!(after.updated_at, settled.updated_at, "then nothing");
}

/// When the owner's record goes, the transaction takes the next owner's
/// fields, once.
#[tokio::test]
async fn next_connection_takes_over_when_the_owner_record_vanishes() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME STORE", false)],
        &[],
        window(),
    )
    .await;
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND TEXT", false)],
        &[],
        window(),
    )
    .await;

    advance(&world);
    let report = apply(&world, &first, Vec::new(), &["a"], Coverage::Delta).await;
    assert_eq!(report.updated, 1);

    let after = only_transaction(&world, account.id).await;
    assert_eq!(after.sources.len(), 1);
    assert_eq!(after.raw_description.as_str(), "SECOND TEXT");
    let audit = transaction_audit(&world).await;
    assert_eq!(imported_updates(&audit, after.id), 1);

    advance(&world);
    let report = apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND TEXT", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.updated, 0);
}

/// A later connection's record going leaves the owner's fields alone.
#[tokio::test]
async fn other_record_vanishing_changes_nothing() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME STORE", false)],
        &[],
        window(),
    )
    .await;
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND TEXT", false)],
        &[],
        window(),
    )
    .await;

    advance(&world);
    let report = apply(&world, &second, Vec::new(), &["b"], Coverage::Delta).await;
    assert_eq!(report.updated, 0);

    let after = only_transaction(&world, account.id).await;
    assert_eq!(after.raw_description.as_str(), "ACME STORE");
    assert_eq!(after.sources.len(), 1);
}

/// A removed connection reports nothing again, so it owns nothing: the
/// connection that remains can correct what it left behind.
#[tokio::test]
async fn removed_connection_owns_nothing() {
    let world = world().await;
    let (first, second, account) = shared_purchase(&world).await;
    apply(
        &world,
        &first,
        vec![record("a", 10, -500, "ACME STORE", false)],
        &[],
        window(),
    )
    .await;
    apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND TEXT", false)],
        &[],
        window(),
    )
    .await;
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        first.connection,
        ConnectionStatusChange {
            status: ConnectionStatus::Removed,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Nothing moves until the remaining connection reports something new.
    advance(&world);
    let report = apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND TEXT", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.updated, 0);
    assert_eq!(
        only_transaction(&world, account.id)
            .await
            .raw_description
            .as_str(),
        "ACME STORE"
    );

    advance(&world);
    let report = apply(
        &world,
        &second,
        vec![record("b", 10, -500, "SECOND CHANGED", false)],
        &[],
        window(),
    )
    .await;
    assert_eq!(report.updated, 1);
    assert_eq!(
        only_transaction(&world, account.id)
            .await
            .raw_description
            .as_str(),
        "SECOND CHANGED"
    );
}
