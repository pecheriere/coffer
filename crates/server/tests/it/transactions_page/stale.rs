//! A form sent after the transaction changed: the details form sends only what it changed from
//! what the page showed, and refuses a field that changed on both sides; a part filed in a
//! category archived since is shown and sent back as it is.

use axum::http::StatusCode;
use coffer_domain::ids::TransactionId;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{Description, Payee};
use coffer_server::ledger::categories::CategoryPatch;
use coffer_server::ledger::transactions::{Patch, TransactionPatch};
use coffer_server::testkit::browser::Reply;
use coffer_store::ImportedFields;
use jiff::civil::date;
use serde_json::json;

use super::{World, assert_redirect, detail, member, world};

/// The value of the hidden field `name` the page drew.
fn shown(page: &Reply, name: &str) -> String {
    page.between(&format!("name=\"{name}\" value=\""), "\"")
}

impl World {
    /// A sync reporting `payee` for the imported transaction `id`, as the bank now names it.
    async fn bank_renames(&self, id: TransactionId, payee: &str) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.update_imported_fields(
            id,
            &ImportedFields {
                posted_on: date(2026, 10, 3),
                transacted_on: Some(date(2026, 10, 2)),
                pending: false,
                amount_minor: -900,
                description: Description::parse("CARD 0042").unwrap(),
                payee: Some(Payee::parse(payee).unwrap()),
                memo: None,
            },
            self.server.clock.now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// Changes `id` as someone else would, through the service.
    async fn meanwhile(&self, id: TransactionId, patch: TransactionPatch) {
        self.server
            .ledger
            .update_transaction(&self.alice, id, patch)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_payee_the_bank_changed_meanwhile_is_not_pinned_by_a_notes_edit() {
    let mut world = world().await;
    let checking = super::account(&world.server, &world.alice, "Checking", "USD").await;
    let id = world.imported(checking, -900, "Corner shop").await;
    let page = world.page(&detail(id)).await;
    world.bank_renames(id, "Corner shop ltd").await;

    let payee_was = shown(&page, "payee_was");
    assert_eq!(payee_was, "Corner shop");
    let reply = world
        .post(
            &format!("{}/details", detail(id)),
            &[
                ("payee", &payee_was),
                ("payee_was", &payee_was),
                ("notes", "Paid in cash"),
                ("notes_was", &shown(&page, "notes_was")),
                ("assignee", ""),
                ("assignee_was", &shown(&page, "assignee_was")),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["notes"], "Paid in cash");
    assert_eq!(read["payee"], "Corner shop ltd", "the bank's new payee");
    let event = &world.audit(id).await[0];
    let fields: Vec<&str> = event.changes.iter().map(|(name, _)| name).collect();
    assert_eq!(fields, ["notes"], "only the notes were sent");

    world.bank_renames(id, "Corner shop plc").await;
    assert_eq!(
        world.read(id).await["payee"],
        "Corner shop plc",
        "a sync still changes it"
    );
}

#[tokio::test]
async fn an_assignee_set_meanwhile_stays_when_only_the_payee_changes() {
    let mut world = world().await;
    let (_, bob) = member(&world).await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let page = world.page(&detail(id)).await;
    world
        .meanwhile(
            id,
            TransactionPatch {
                assignee: Patch::Set(bob.member),
                ..TransactionPatch::default()
            },
        )
        .await;

    let reply = world
        .post(
            &format!("{}/details", detail(id)),
            &[
                ("payee", "Corner bakery"),
                ("payee_was", &shown(&page, "payee_was")),
                ("notes", ""),
                ("notes_was", &shown(&page, "notes_was")),
                ("assignee", ""),
                ("assignee_was", &shown(&page, "assignee_was")),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["payee"], "Corner bakery");
    assert_eq!(read["assignee"], bob.member.to_string(), "kept");
}

#[tokio::test]
async fn a_field_changed_here_and_meanwhile_is_refused_whole() {
    let mut world = world().await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let page = world.page(&detail(id)).await;
    world
        .meanwhile(
            id,
            TransactionPatch {
                notes: Patch::Set("Note from Bob".into()),
                ..TransactionPatch::default()
            },
        )
        .await;
    let events = world.audit(id).await.len();

    let reply = world
        .post(
            &format!("{}/details", detail(id)),
            &[
                ("payee", "xyzzy-payee"),
                ("payee_was", &shown(&page, "payee_was")),
                ("notes", "xyzzy-notes"),
                ("notes_was", &shown(&page, "notes_was")),
                ("assignee", ""),
                ("assignee_was", &shown(&page, "assignee_was")),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(
        reply
            .body
            .contains("It changed since you opened it, so nothing was saved.")
    );
    assert!(
        reply.body.contains("Note from Bob"),
        "shows it as it is now"
    );
    assert!(!reply.body.contains("xyzzy"), "never echoes the input");
    let read = world.read(id).await;
    assert_eq!(read["payee"], "Bakery", "nothing saved");
    assert_eq!(read["notes"], "Note from Bob");
    assert_eq!(world.audit(id).await.len(), events, "nothing audited");
}

#[tokio::test]
async fn a_part_in_a_category_archived_since_is_sent_back_unchanged() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let id = world.manual(-1_250, "Bakery", date(2026, 10, 2)).await;
    let other = world.manual(-300, "Kiosk", date(2026, 10, 2)).await;
    let reply = world
        .post(
            &format!("{}/category", detail(id)),
            &[("category", &groceries.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    world
        .server
        .ledger
        .update_category(
            &world.alice,
            groceries,
            CategoryPatch {
                archived: Some(true),
                ..CategoryPatch::default()
            },
        )
        .await
        .unwrap();

    let page = world.page(&detail(id)).await;
    assert!(page.body.contains(&format!(
        "<option value=\"{groceries}\" selected>Groceries group: Groceries (archived)</option>"
    )));
    let reply = world
        .post(
            &format!("{}/splits", detail(id)),
            &[
                ("category_0", &groceries.to_string()),
                ("amount_0", "12.50"),
            ],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let reply = world
        .post(
            &format!("{}/category", detail(id)),
            &[("category", &groceries.to_string()), ("reviewed", "yes")],
        )
        .await;
    assert_redirect(&reply, &detail(id));
    let read = world.read(id).await;
    assert_eq!(read["splits"][0]["category"], groceries.to_string());
    assert_eq!(read["review"], "reviewed");

    let page = world.page(&detail(other)).await;
    assert!(!page.body.contains("(archived)"), "not offered elsewhere");
    assert_eq!(world.read(other).await["splits"], json!([]));
}
