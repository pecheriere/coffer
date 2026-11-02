//! A held sync cursor on the page: what it says, what a release loses, and
//! the form that lets the cursor move on.

use axum::http::{StatusCode, header};
use coffer_domain::identity::Role;
use coffer_domain::ids::ConnectionId;
use coffer_server::sync::connections::{CURSOR_HELD_TEXT, HELD_AFTER_SYNCS};
use coffer_server::testkit::browser::Browser;
use coffer_store::ProviderAccountRecord;

use super::{World, assert_inline_free, world};

const LOSS: &str = "passed for good";

fn release_path(connection: ConnectionId) -> String {
    format!("/connections/{connection}/release-cursor")
}

impl World {
    /// A provider account of `connection` whose records have held the cursor
    /// for `syncs` syncs.
    async fn held(
        &self,
        connection: ConnectionId,
        name: &str,
        syncs: u32,
    ) -> ProviderAccountRecord {
        let account = self.report(connection, name, "USD", None).await;
        let mut tx = self.server.store.begin().await.unwrap();
        for _ in 0..syncs {
            tx.count_cursor_hold(account.id, 4).await.unwrap();
        }
        tx.commit().await.unwrap();
        account
    }

    async fn release_audits(&self) -> usize {
        self.server
            .store
            .list_audit(
                &coffer_store::AuditFilter::default(),
                &coffer_store::PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .iter()
            .filter(|event| event.action == "connection.release_cursor")
            .count()
    }
}

#[tokio::test]
async fn a_cursor_that_is_not_held_shows_nothing() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world
        .held(connection, "Savings", HELD_AFTER_SYNCS - 1)
        .await;

    let page = world.admin.get("/connections").await;

    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains(CURSOR_HELD_TEXT));
    assert!(!page.body.contains(&release_path(connection)));
    assert!(!page.body.contains("Release requested"));
}

#[tokio::test]
async fn a_held_cursor_names_its_accounts_and_says_what_a_release_loses() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world
        .held(connection, "Broken <b>Savings</b>", HELD_AFTER_SYNCS)
        .await;
    world.held(connection, "Fine", 1).await;

    let page = world.admin.get("/connections").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    assert!(page.body.contains(CURSOR_HELD_TEXT));
    assert!(
        page.body.contains("Broken &lt;b&gt;Savings&lt;/b&gt;"),
        "the cause is named, escaped"
    );
    let held_part = page.between("held-cursor", "</form>");
    assert!(!held_part.contains("Fine"), "only accounts holding it");
    assert!(page.body.contains(LOSS), "the page says what is lost");
    assert!(
        page.body.contains("only counted as skipped"),
        "and that they are only counted"
    );
    assert!(
        page.body.contains("one by one") && page.body.contains("Those that can be stored"),
        "and that only the ones that still fail are passed"
    );
    assert!(
        page.body.contains("The last fetch held 4 records"),
        "and how many the last fetch held"
    );
    assert!(
        page.body
            .contains(&format!("action=\"{}\"", release_path(connection)))
    );
}

#[tokio::test]
async fn releasing_asks_for_the_token_and_changes_nothing_without_it() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let account = world.held(connection, "Savings", HELD_AFTER_SYNCS).await;
    let page = world.admin.get("/connections").await;
    let shown = account.id.to_string();

    let missing = world
        .admin
        .post_form(&release_path(connection), &[("accounts", &shown)])
        .await;
    let wrong = world
        .admin
        .post_form(
            &release_path(connection),
            &[("csrf", "AAAA"), ("accounts", &shown)],
        )
        .await;
    let forged = world
        .admin
        .post_form(
            &release_path(connection),
            &[("csrf", &page.csrf()), ("accounts", &shown), ("all", "yes")],
        )
        .await;
    let unreadable = world
        .admin
        .post_form(
            &release_path(connection),
            &[("csrf", &page.csrf()), ("accounts", "everything")],
        )
        .await;

    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_ne!(forged.status, StatusCode::SEE_OTHER, "unknown fields fail");
    assert_eq!(unreadable.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!world.provider_account("Savings").await.hold_released);
    assert_eq!(world.release_audits().await, 0);
}

/// What the member saw is what is released: a sync that added an account
/// since the page was loaded makes the release refuse, with nothing changed.
#[tokio::test]
async fn releasing_refuses_when_the_accounts_changed_since_the_page() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let first = world.held(connection, "Savings", HELD_AFTER_SYNCS).await;
    let page = world.admin.get("/connections").await;
    assert!(
        page.body.contains(&format!("value=\"{}\"", first.id)),
        "the page posts the accounts it shows"
    );
    world.held(connection, "Newcomer", HELD_AFTER_SYNCS).await;

    let refused = world
        .admin
        .post_form(
            &release_path(connection),
            &[("csrf", &page.csrf()), ("accounts", &first.id.to_string())],
        )
        .await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(!world.provider_account("Savings").await.hold_released);
    assert!(!world.provider_account("Newcomer").await.hold_released);
    assert_eq!(world.release_audits().await, 0);
}

#[tokio::test]
async fn releasing_is_audited_and_waits_for_the_next_sync() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.held(connection, "Savings", HELD_AFTER_SYNCS).await;
    let page = world.admin.get("/connections").await;
    let shown = world.provider_account("Savings").await.id.to_string();

    let done = world
        .admin
        .post_form(
            &release_path(connection),
            &[("csrf", &page.csrf()), ("accounts", &shown)],
        )
        .await;

    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(done.header(header::LOCATION), "/connections");
    assert!(world.provider_account("Savings").await.hold_released);
    assert_eq!(world.release_audits().await, 1);
    let after = world.admin.get("/connections").await;
    assert!(after.body.contains(CURSOR_HELD_TEXT));
    assert!(
        after.body.contains("Release requested"),
        "the page says when the cursor moves"
    );
    assert!(
        !after.body.contains(&release_path(connection)),
        "nothing left to ask for"
    );
}

#[tokio::test]
async fn releasing_a_cursor_that_is_not_held_says_so() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let page = world.admin.get("/connections").await;

    let refused = world
        .admin
        .post_form(&release_path(connection), &[("csrf", &page.csrf())])
        .await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(refused.body.contains("not held"));
    assert_eq!(world.release_audits().await, 0);
}

#[tokio::test]
async fn a_member_who_did_not_add_the_connection_cannot_release_it() {
    let world = world().await;
    let connection = world.seeded(world.alice).await;
    world.held(connection, "Savings", HELD_AFTER_SYNCS).await;
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

    let page = bob.get("/connections").await;
    assert!(
        !page.body.contains(&release_path(connection)),
        "no form for what he may not do"
    );
    let shown = world.provider_account("Savings").await.id.to_string();
    let refused = bob
        .post_form(
            &release_path(connection),
            &[("csrf", &page.csrf()), ("accounts", &shown)],
        )
        .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(!world.provider_account("Savings").await.hold_released);
    assert_eq!(world.release_audits().await, 0);
}
