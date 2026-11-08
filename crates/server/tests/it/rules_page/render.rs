//! What the list and a rule's page show.

use axum::http::StatusCode;
use serde_json::json;

use super::{filed_under, world};

#[tokio::test]
async fn an_empty_list_says_so_and_offers_the_form() {
    let mut world = world().await;

    let page = world.browser.get("/rules").await;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No rules yet."));
    assert!(page.body.contains(r#"action="/rules""#));
    assert!(
        page.body
            .contains(r#"<a href="/rules" aria-current="page">Rules</a>"#)
    );
}

#[tokio::test]
async fn the_list_names_each_rule_s_text_account_category_and_state() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let first = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    world
        .rule(json!({
            "match_kind": "starts_with", "pattern": "TAX ", "account": everyday,
            "amount": {"currency": "USD", "min_minor": -5000, "max_minor": -1000},
            "category": groceries, "enabled": false,
        }))
        .await;

    let page = world.browser.get("/rules").await;

    assert_eq!(page.status, StatusCode::OK);
    let list = page.between(r#"id="rules""#, "</table>");
    assert!(list.contains(&format!(r#"href="/rules/{first}""#)));
    assert!(list.contains("contains \u{201c}market\u{201d}"));
    assert!(list.contains("starts with \u{201c}TAX \u{201d}"));
    assert!(list.contains("Every account"));
    assert!(list.contains("Everyday, amounts from -50.00 to -10.00 USD"));
    assert!(list.contains("files under Groceries"));
    assert!(list.contains("Off"));
    let market = list.find("market").unwrap();
    let tax = list.find("TAX").unwrap();
    assert!(market < tax, "in the order they run");
}

#[tokio::test]
async fn a_rule_s_page_shows_what_running_it_would_change_and_writes_nothing() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let open = world
        .transaction(everyday, -4_250, "CORNER MARKET 42")
        .await;
    let reviewed = world.transaction(everyday, -1_000, "MARKET HALL").await;
    world.review(reviewed).await;
    world.transaction(everyday, -900, "BAKERY").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;

    let page = world.browser.get(&format!("/rules/{rule}")).await;

    assert_eq!(page.status, StatusCode::OK);
    let trial = page.between(r#"id="dry-run""#, "</section>");
    assert!(trial.contains("Running it now would change 1 transaction"));
    assert!(trial.contains("It would file 1 transaction under Groceries."));
    assert!(trial.contains("CORNER MARKET 42"));
    assert!(trial.contains("-42.50 USD"));
    assert!(trial.contains("10 October 2026"));
    assert!(!trial.contains("BAKERY"), "only what it matches");
    assert!(trial.contains("1 transaction someone has reviewed"));
    assert!(trial.contains(&format!(r#"action="/rules/{rule}/apply""#)));
    assert_eq!(filed_under(&world.read_transaction(open).await), None);
}

#[tokio::test]
async fn an_unknown_or_malformed_rule_is_not_found() {
    let mut world = world().await;

    let unknown = world
        .browser
        .get("/rules/0190f5b4-0000-7000-8000-000000000000")
        .await;
    let malformed = world.browser.get("/rules/not-an-id").await;

    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(malformed.status, StatusCode::NOT_FOUND);
}
