//! The reminder on every signed-in page of the provider accounts that wait
//! to be set up, and the guide on the Connections page.

use axum::http::StatusCode;
use coffer_domain::accounts::AccountKind;
use coffer_server::testkit::captured_log_text;

use super::bind::create_path;
use super::{ACCOUNTS_FIRST, assert_inline_free, world};

const SETUP_ONE: &str = "1 new bank account needs setting up.";
const SETUP_TWO: &str = "2 new bank accounts need setting up.";

#[tokio::test]
async fn every_signed_in_page_counts_the_accounts_to_set_up() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    assert!(
        !world
            .admin
            .get("/account")
            .await
            .body
            .contains("need setting up"),
        "nothing to set up yet"
    );
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;

    let account = world.admin.get("/account").await;
    assert!(account.body.contains(SETUP_TWO));
    assert!(account.body.contains("href=\"/connections\">Set them up<"));
    assert_inline_free(&account);
    assert!(
        world
            .admin
            .get("/admin/members")
            .await
            .body
            .contains(SETUP_TWO)
    );
    assert!(
        !world
            .admin
            .get("/connections")
            .await
            .body
            .contains(SETUP_TWO),
        "the Connections page has the forms themselves"
    );

    world
        .admin
        .post_form(
            &create_path(checking.id),
            &[
                ("csrf", &account.csrf()),
                ("name", "Everyday"),
                ("kind", "checking"),
            ],
        )
        .await;
    let one = world.admin.get("/account").await;
    assert!(one.body.contains(SETUP_ONE));
    assert!(!one.body.contains(SETUP_TWO));

    let card = world.provider_account("Credit Card").await;
    world
        .server
        .sync
        .create_account_and_bind(
            &world.alice,
            card.id,
            coffer_server::sync::connections::NewLinkedAccountInput {
                name: None,
                kind: Some(AccountKind::CreditCard),
                liquidity: None,
                include_in_net_worth: true,
                include_in_forecast: true,
            },
        )
        .await
        .unwrap();
    assert!(
        !world
            .admin
            .get("/account")
            .await
            .body
            .contains("need setting up"),
        "the notice goes once every account is bound"
    );

    // A later sync reports an account that is new.
    let later = ACCOUNTS_FIRST
        .replace("ACT-0002", "ACT-0003")
        .replace("Credit Card", "Travel Card");
    world.sync_reply(connection, &later).await;
    assert!(world.admin.get("/account").await.body.contains(SETUP_ONE));
}

#[tokio::test]
async fn accounts_of_a_removed_connection_are_not_counted() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let csrf = world.admin.get("/account").await.csrf();
    world
        .admin
        .post_form(
            &format!("/connections/{connection}/remove"),
            &[("csrf", &csrf)],
        )
        .await;

    assert!(
        !world
            .admin
            .get("/account")
            .await
            .body
            .contains("need setting up")
    );
}

#[tokio::test]
async fn a_failed_count_leaves_the_notice_out_and_the_page_up() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    world
        .server
        .store
        .execute_unchecked_for_tests("ALTER TABLE provider_accounts RENAME TO gone_for_test", "")
        .await
        .unwrap();

    let page = world.admin.get("/account").await;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Account"));
    assert!(!page.body.contains("need setting up"));
    assert!(captured_log_text().contains("could not count the provider accounts to set up"));
}

#[tokio::test]
async fn the_first_sync_walks_through_the_accounts_it_found() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;

    let page = world.admin.get("/connections").await;

    assert!(page.body.contains("Set up your accounts"));
    assert!(page.body.contains("Until each is bound"));
    assert!(
        page.body
            .contains("Checking and savings count as cash for planning."),
        "the kind's effect is explained"
    );
    let select = page.between("name=\"kind\"", "</select>");
    assert!(
        select.contains("<option value=\"\" selected disabled>Choose a kind</option>"),
        "without a hint the kind is a choice to make, not a default"
    );
    assert!(!select.contains("<option value=\"checking\" selected"));

    world
        .admin
        .post_form(
            &create_path(checking.id),
            &[("csrf", &page.csrf()), ("name", "A"), ("kind", "checking")],
        )
        .await;
    let card = world.provider_account("Credit Card").await;
    world
        .admin
        .post_form(
            &create_path(card.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "B"),
                ("kind", "credit_card"),
            ],
        )
        .await;
    let done = world.admin.get("/connections").await;
    assert!(!done.body.contains("Set up your accounts"));
    assert!(done.body.contains("Bound to A"));
}
