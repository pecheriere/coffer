//! Dismissing an unbound provider account from the page, and undoing it: the forms ask the sync
//! service, and a dismissed account stays on the page, marked, and can still be bound.

use axum::http::{StatusCode, header};
use coffer_domain::ids::ProviderAccountId;

use super::bind::create_path;
use super::{assert_inline_free, world};

fn dismiss_path(id: ProviderAccountId) -> String {
    format!("/connections/provider-accounts/{id}/dismiss")
}

fn restore_path(id: ProviderAccountId) -> String {
    format!("/connections/provider-accounts/{id}/restore")
}

#[tokio::test]
async fn a_dismissed_account_stays_listed_marked_with_an_undo() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let card = world.provider_account("Credit Card").await;

    let page = world.admin.get("/connections").await;
    assert_inline_free(&page);
    for id in [checking.id, card.id] {
        assert!(
            page.body
                .contains(&format!("action=\"{}\"", dismiss_path(id))),
            "an unbound account offers to be dismissed"
        );
    }
    assert!(!page.body.contains("Dismissed"));

    let done = world
        .admin
        .post_form(&dismiss_path(checking.id), &[("csrf", &page.csrf())])
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(done.header(header::LOCATION), "/connections");

    let after = world.admin.get("/connections").await;
    assert!(after.body.contains("Checking"), "it stays listed");
    assert_eq!(after.body.matches("Dismissed").count(), 1);
    assert!(
        after
            .body
            .contains(&format!("action=\"{}\"", restore_path(checking.id))),
        "with an undo"
    );
    assert!(
        !after
            .body
            .contains(&format!("action=\"{}\"", dismiss_path(checking.id))),
        "and no second dismissal"
    );
    assert!(
        after
            .body
            .contains(&format!("action=\"{}\"", create_path(checking.id))),
        "it can still be bound"
    );
    assert!(
        after.body.contains("Set up your accounts"),
        "the other account still waits"
    );

    let undone = world
        .admin
        .post_form(&restore_path(checking.id), &[("csrf", &after.csrf())])
        .await;
    assert_eq!(undone.status, StatusCode::SEE_OTHER);
    let restored = world.admin.get("/connections").await;
    assert!(!restored.body.contains("Dismissed"));
    assert!(
        restored
            .body
            .contains(&format!("action=\"{}\"", dismiss_path(checking.id)))
    );
}

#[tokio::test]
async fn a_page_older_than_the_change_gets_a_conflict_notice() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let page = world.admin.get("/connections").await;
    let csrf = page.csrf();
    world
        .admin
        .post_form(&dismiss_path(checking.id), &[("csrf", &csrf)])
        .await;

    let stale = world
        .admin
        .post_form(&dismiss_path(checking.id), &[("csrf", &csrf)])
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert!(stale.body.contains("already dismissed"));

    let early = world
        .admin
        .post_form(
            &restore_path(world.provider_account("Credit Card").await.id),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(early.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn dismissing_something_unknown_is_not_found() {
    let mut world = world().await;
    let page = world.admin.get("/connections").await;
    let missing = world
        .admin
        .post_form(
            &dismiss_path(ProviderAccountId::new()),
            &[("csrf", &page.csrf())],
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}
