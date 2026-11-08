//! What the forms refuse, in fixed words that never repeat what was sent, and what a member
//! may do, which is what the API lets a member do.

use axum::http::StatusCode;
use serde_json::json;

use super::{by_member, csrf_of, filed_under, world};

/// The fields of an add form filing what contains `pattern` under `category`.
fn adding<'a>(csrf: &'a str, pattern: &'a str, category: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("csrf", csrf),
        ("match_kind", "contains"),
        ("pattern", pattern),
        ("account", ""),
        ("amount_min", ""),
        ("amount_max", ""),
        ("amount_currency", ""),
        ("category", category),
        ("assignee", ""),
        ("position", ""),
        ("enabled", "yes"),
    ]
}

#[tokio::test]
async fn a_bad_amount_is_refused_without_repeating_it() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await.to_string();
    let csrf = csrf_of(&mut world.browser, "/rules").await;
    let mut fields = adding(&csrf, "market", &groceries);
    fields.retain(|(name, _)| !matches!(*name, "amount_min" | "amount_currency"));
    fields.push(("amount_min", "12.345<b>sly</b>"));
    fields.push(("amount_currency", "USD"));

    let refused = world.browser.post_form("/rules", &fields).await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains("an amount is a number such as -12.50")
    );
    assert!(!refused.body.contains("sly"), "the input is not echoed");
    assert_eq!(world.rules().await.len(), 0);
}

#[tokio::test]
async fn amounts_on_every_account_need_a_currency() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await.to_string();
    let csrf = csrf_of(&mut world.browser, "/rules").await;
    let mut fields = adding(&csrf, "market", &groceries);
    fields.retain(|(name, _)| *name != "amount_max");
    fields.push(("amount_max", "-10.00"));

    let refused = world.browser.post_form("/rules", &fields).await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains("amounts on every account need a currency")
    );
    assert_eq!(world.rules().await.len(), 0);
}

#[tokio::test]
async fn the_ledger_s_refusals_come_back_in_its_fixed_words() {
    let mut world = world().await;
    let csrf = csrf_of(&mut world.browser, "/rules").await;

    let nothing = world
        .browser
        .post_form("/rules", &adding(&csrf, "market", ""))
        .await;
    let control = world
        .browser
        .post_form("/rules", &adding(&csrf, "mark\u{7}et", ""))
        .await;
    let unknown = world
        .browser
        .post_form(
            "/rules",
            &adding(&csrf, "market", "0190f5b4-0000-7000-8000-000000000000"),
        )
        .await;

    assert_eq!(nothing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        nothing
            .body
            .contains("a rule sets a category, an assignee or both")
    );
    assert_eq!(control.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(control.body.contains("pattern must be 1 to 200 characters"));
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(unknown.body.contains("the category does not exist"));
    assert_eq!(world.rules().await.len(), 0);
}

#[tokio::test]
async fn a_forged_field_or_a_missing_token_is_refused() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await.to_string();
    let csrf = csrf_of(&mut world.browser, "/rules").await;
    let mut forged = adding(&csrf, "market", &groceries);
    forged.push(("review", "reviewed"));

    let extra = world.browser.post_form("/rules", &forged).await;
    let unsigned = world
        .browser
        .post_form("/rules", &adding("", "market", &groceries))
        .await;

    assert_eq!(extra.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unsigned.status, StatusCode::FORBIDDEN);
    assert_eq!(world.rules().await.len(), 0);

    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    for action in ["apply", "undo", "delete"] {
        let refused = world
            .browser
            .post_form(&format!("/rules/{rule}/{action}"), &[("csrf", "")])
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let forged = world
            .browser
            .post_form(
                &format!("/rules/{rule}/{action}"),
                &[("csrf", &csrf), ("rule", "other")],
            )
            .await;
        assert_eq!(forged.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(world.rules().await.len(), 1);
    assert_eq!(world.events("rule.apply").await.len(), 0);
}

/// A member may keep and run rules through the API, so the pages let them too, audited as them.
#[tokio::test]
async fn a_member_adds_and_runs_a_rule_as_the_api_allows() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let (bob, mut browser) = world.member_bob().await;
    let page = browser.get("/rules").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(r#"id="add-rule""#));
    let csrf = page.csrf();

    let groceries_text = groceries.to_string();
    let added = browser
        .post_form("/rules", &adding(&csrf, "market", &groceries_text))
        .await;
    assert_eq!(added.status, StatusCode::SEE_OTHER);
    let rule = world.rules().await[0]["id"].as_str().unwrap().to_owned();
    let ran = browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;

    assert_eq!(ran.status, StatusCode::SEE_OTHER);
    assert_eq!(
        filed_under(&world.read_transaction(open).await),
        Some(groceries_text)
    );
    assert!(by_member(&world.events("rule.create").await[0], bob));
    assert!(by_member(&world.events("rule.apply").await[0], bob));
}
