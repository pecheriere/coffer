//! A change form older than the rule: it sends only what the person changed, so someone else's
//! change meanwhile stays, and a field both changed is refused in fixed words.

use axum::http::{Method, StatusCode};
use serde_json::json;

use super::{change_form, post_owned, world};

#[tokio::test]
async fn a_stale_form_keeps_what_someone_else_changed_in_another_field() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let dining = world.category("Dining").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let page = world.browser.get(&format!("/rules/{rule}")).await;
    let (status, _) = world
        .api(
            Method::PATCH,
            &format!("/api/v1/rules/{rule}"),
            Some(json!({"category": dining})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let fields = change_form(&page, &[("enabled", "")]);
    let saved = post_owned(&mut world.browser, &format!("/rules/{rule}"), &fields).await;

    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    let rules = world.rules().await;
    assert_eq!(
        rules[0]["category"],
        dining.to_string(),
        "their change stays"
    );
    assert_eq!(rules[0]["enabled"], false, "the change asked for is made");
    let updates = world.events("rule.update").await;
    assert_eq!(updates.len(), 2, "theirs, then this one");
}

#[tokio::test]
async fn a_stale_form_changing_what_changed_meanwhile_is_refused() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let dining = world.category("Dining").await;
    let travel = world.category("Travel").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let page = world.browser.get(&format!("/rules/{rule}")).await;
    let (status, _) = world
        .api(
            Method::PATCH,
            &format!("/api/v1/rules/{rule}"),
            Some(json!({"category": dining})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let travel_text = travel.to_string();
    let fields = change_form(
        &page,
        &[("category", &travel_text), ("pattern", "zzqx market")],
    );
    let refused = post_owned(&mut world.browser, &format!("/rules/{rule}"), &fields).await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(
        refused
            .body
            .contains("Someone changed this rule after the page opened")
    );
    assert!(
        !refused.body.contains("zzqx"),
        "what was sent isn't repeated"
    );
    assert!(
        !refused.body.contains(&travel_text),
        "nor the category sent"
    );
    let rules = world.rules().await;
    assert_eq!(rules[0]["category"], dining.to_string(), "nothing changed");
    assert_eq!(rules[0]["pattern"], "market", "not even the other field");
    assert_eq!(world.events("rule.update").await.len(), 1, "only theirs");
}

#[tokio::test]
async fn saving_another_change_keeps_an_archived_category_the_rule_names() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let (status, _) = world
        .api(
            Method::PATCH,
            &format!("/api/v1/categories/{groceries}"),
            Some(json!({"archived": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let page = world.browser.get(&format!("/rules/{rule}")).await;
    let form = page.between(r#"id="change""#, "</form>");
    assert!(form.contains("Groceries (archived)"), "still offered");

    let fields = change_form(&page, &[("pattern", "corner market")]);
    let saved = post_owned(&mut world.browser, &format!("/rules/{rule}"), &fields).await;

    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    let rules = world.rules().await;
    assert_eq!(rules[0]["pattern"], "corner market");
    assert_eq!(rules[0]["category"], groceries.to_string());
    assert_eq!(rules[0]["category_archived"], true);
}

#[tokio::test]
async fn saving_an_untouched_form_changes_nothing() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let rule = world
        .rule(json!({
            "match_kind": "starts_with", "pattern": "TAX ", "account": everyday,
            "amount": {"currency": "USD", "min_minor": -5000, "max_minor": null},
            "category": groceries, "assignee": world.alice.member, "position": 4,
        }))
        .await;
    let before = world.rules().await;
    let page = world.browser.get(&format!("/rules/{rule}")).await;

    let fields = change_form(&page, &[]);
    let saved = post_owned(&mut world.browser, &format!("/rules/{rule}"), &fields).await;

    assert_eq!(saved.status, StatusCode::SEE_OTHER);
    assert_eq!(world.rules().await, before);
    assert_eq!(
        world.events("rule.update").await.len(),
        0,
        "nothing to audit"
    );
}
