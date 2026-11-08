//! Home at `/`: net worth with its change this month, and how many transactions wait to be
//! checked, from the same ledger calls the API makes.

use axum::http::{Method, StatusCode};
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use serde_json::json;

use super::forecast::{section, text_of};
use super::get_and_head;
use super::net_worth::{alice, bob};
use crate::net_worth::{World, world};

const NO_CURRENCY: &str = "Choose a display currency to see net worth in one number.";
const NOTHING_COUNTED: &str = "No accounts count towards net worth yet.";

/// Home as alice, which must answer 200.
async fn home(world: &World) -> Reply {
    let reply = alice(world).await.get("/").await;
    assert_eq!(reply.status, StatusCode::OK);
    reply
}

/// `count` manual transactions in a new account, none reviewed.
async fn unreviewed(world: &World, count: usize) {
    let holder = world.holding("Everyday", "USD").await;
    for _ in 0..count {
        world
            .admin_send(
                Method::POST,
                "/api/v1/transactions",
                &json!({"account": holder, "posted_on": "2026-11-01",
                        "minor_units": -1250, "description": "Bakery"}),
            )
            .await;
    }
}

#[tokio::test]
async fn root_is_home_not_a_redirect() {
    let (_server, mut admin, _) = with_admin().await;
    for reply in get_and_head(&mut admin, "/").await {
        assert_eq!(reply.status, StatusCode::OK);
    }
    let page = admin.get("/").await;
    assert!(page.body.contains("<h1>Home</h1>"));
    assert!(page.body.contains("aria-current=\"page\">Home<"));
}

#[tokio::test]
async fn sign_in_lands_on_home() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut key = server.authenticator();
    let enrolled = Browser::new(&server)
        .enroll(&mut key, &link, "alice", "Alice")
        .await;
    assert_eq!(enrolled.status, StatusCode::OK);
    assert_eq!(enrolled.json()["redirect"], "/");

    let again = Browser::new(&server).login(&mut key, "alice").await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["redirect"], "/");
}

#[tokio::test]
async fn account_page_still_answers() {
    let (_server, mut admin, _) = with_admin().await;
    let page = admin.get("/account").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("aria-current=\"page\">More<"));
}

#[tokio::test]
async fn home_shows_net_worth_and_its_change_this_month() {
    let world = world().await;
    let golden = world.golden().await;
    world.balance(golden.checking, "2026-11-01", 560_000).await;

    let api = world
        .report("from=2025-11-01&interval=month&account_points=false")
        .await;
    let points = api["points"].as_array().unwrap();
    let [.., october, november] = points.as_slice() else {
        panic!("October and November have values");
    };
    assert_eq!(october["total"]["minor_units"], 631_720);
    assert_eq!(november["total"]["minor_units"], 641_720);

    let page = home(&world).await;
    let card = text_of(section(&page.body, "net-worth"));
    assert!(card.contains("6,417.20 USD"), "{card}");
    assert!(card.contains("Up 100.00 USD this month"), "{card}");
    assert_eq!(page.body.matches("<polyline").count(), 1);
    assert!(page.body.contains("href=\"/net-worth\">See its history<"));
}

#[tokio::test]
async fn home_without_a_display_currency_says_so() {
    let world = world().await;
    let holder = world.holding("Everyday", "USD").await;
    world.balance(holder, "2026-11-01", 10_000).await;

    let page = home(&world).await;
    let card = section(&page.body, "net-worth");
    assert!(text_of(card).contains(NO_CURRENCY));
    assert!(card.contains("href=\"/admin/household\""));
    assert!(
        !card.contains("0.00"),
        "no number stands in for the missing one"
    );

    let mut bob = bob(&world).await;
    let page = bob.get("/").await;
    let card = section(&page.body, "net-worth");
    assert!(text_of(card).contains(NO_CURRENCY));
    assert!(card.contains("href=\"/net-worth\""));
    assert!(!card.contains("href=\"/admin/household\""));
}

#[tokio::test]
async fn home_net_worth_follows_what_the_viewer_sees() {
    let world = world().await;
    let page = home(&world).await;
    let card = section(&page.body, "net-worth");
    assert!(text_of(card).contains(NOTHING_COUNTED));
    assert!(card.contains("href=\"/accounts\""));
    assert!(page.body.contains("id=\"to-check\""));

    world.golden().await;
    let mut bob = bob(&world).await;
    let history = bob.get("/net-worth").await;
    assert!(text_of(section(&history.body, "answer")).contains("6,317.20 USD"));
    let page = bob.get("/").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(text_of(section(&page.body, "net-worth")).contains("6,317.20 USD"));
}

#[tokio::test]
async fn home_counts_transactions_to_check() {
    let world = world().await;
    let page = home(&world).await;
    assert!(text_of(section(&page.body, "to-check")).contains("Nothing to check."));

    unreviewed(&world, 1).await;
    let page = home(&world).await;
    assert!(text_of(section(&page.body, "to-check")).contains("1 transaction to check"));

    unreviewed(&world, 2).await;
    let page = home(&world).await;
    let card = section(&page.body, "to-check");
    assert!(text_of(card).contains("3 transactions to check"));
    assert!(card.contains("href=\"/transactions?review=unreviewed\""));
}
