//! Adding, changing and deleting a rule from the pages, read back through the API and audited
//! as the signed-in member.

use axum::http::{StatusCode, header};
use serde_json::json;

use super::{by_member, change_form, csrf_of, filed_under, post_owned, world};

#[tokio::test]
async fn the_add_form_creates_the_rule_it_describes() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let csrf = csrf_of(&mut world.browser, "/rules").await;

    let groceries_text = groceries.to_string();
    let everyday_text = everyday.to_string();
    let alice_text = world.alice.member.to_string();
    let added = world
        .browser
        .post_form(
            "/rules",
            &[
                ("csrf", &csrf),
                ("match_kind", "starts_with"),
                ("pattern", "TAX "),
                ("account", &everyday_text),
                ("amount_min", "-50.00"),
                ("amount_max", "-10"),
                ("amount_currency", ""),
                ("category", &groceries_text),
                ("assignee", &alice_text),
                ("position", "7"),
            ],
        )
        .await;

    assert_eq!(added.status, StatusCode::SEE_OTHER);
    let rules = world.rules().await;
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    let id = rule["id"].as_str().unwrap();
    assert_eq!(added.header(header::LOCATION), format!("/rules/{id}"));
    assert_eq!(rule["match_kind"], "starts_with");
    assert_eq!(rule["pattern"], "TAX ", "kept as written");
    assert_eq!(rule["account"], everyday_text);
    assert_eq!(
        rule["amount"],
        json!({"currency": "USD", "min_minor": -5000, "max_minor": -1000}),
        "in the account's currency"
    );
    assert_eq!(rule["category"], groceries_text);
    assert_eq!(rule["assignee"], alice_text);
    assert_eq!(rule["position"], 7);
    assert_eq!(rule["enabled"], false, "an unticked box turns it off");
    let created = world.events("rule.create").await;
    assert_eq!(created.len(), 1);
    assert!(by_member(&created[0], world.alice.member));
}

#[tokio::test]
async fn the_change_form_changes_what_it_sends_and_leaves_what_the_rule_did() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let filed = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "market", "category": groceries,
            "assignee": world.alice.member,
        }))
        .await;
    let (status, _) = world
        .api(
            axum::http::Method::POST,
            &format!("/api/v1/rules/{rule}/apply"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let page = world.browser.get(&format!("/rules/{rule}")).await;
    let form = page.between(r#"id="change""#, "</form>");
    assert!(form.contains(r#"value="market""#), "filled from the rule");
    assert!(form.contains(r#"name="enabled" value="yes" checked"#));

    let groceries_text = groceries.to_string();
    let fields = change_form(
        &page,
        &[("pattern", "corner"), ("assignee", ""), ("position", "")],
    );
    let changed = post_owned(&mut world.browser, &format!("/rules/{rule}"), &fields).await;

    assert_eq!(changed.status, StatusCode::SEE_OTHER);
    assert_eq!(changed.header(header::LOCATION), format!("/rules/{rule}"));
    let rules = world.rules().await;
    assert_eq!(rules[0]["pattern"], "corner");
    assert_eq!(rules[0]["assignee"], serde_json::Value::Null);
    assert_eq!(rules[0]["position"], 0, "an empty order keeps its place");
    let transaction = world.read_transaction(filed).await;
    assert_eq!(filed_under(&transaction), Some(groceries_text));
    assert_eq!(
        transaction["assignee"],
        world.alice.member.to_string(),
        "what it already filled stays"
    );
    let updated = world.events("rule.update").await;
    assert_eq!(updated.len(), 1);
    assert!(by_member(&updated[0], world.alice.member));
}

#[tokio::test]
async fn the_delete_form_deletes_the_rule_and_leaves_what_it_filled() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let filed = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;
    let ran = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    assert_eq!(ran.status, StatusCode::SEE_OTHER);

    let deleted = world
        .browser
        .post_form(&format!("/rules/{rule}/delete"), &[("csrf", &csrf)])
        .await;

    assert_eq!(deleted.status, StatusCode::SEE_OTHER);
    assert_eq!(deleted.header(header::LOCATION), "/rules");
    assert_eq!(world.rules().await.len(), 0);
    assert_eq!(
        filed_under(&world.read_transaction(filed).await),
        Some(groceries.to_string())
    );
    let gone = world.events("rule.delete").await;
    assert_eq!(gone.len(), 1);
    assert!(by_member(&gone[0], world.alice.member));
}
