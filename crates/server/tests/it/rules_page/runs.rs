//! Running a rule from its page and taking back what it filled, read back through the API and
//! audited as the signed-in member.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{by_member, csrf_of, filed_under, world};

#[tokio::test]
async fn running_fills_only_what_nobody_set_and_says_what_it_did() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let dining = world.category("Dining").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let chosen = world.transaction(everyday, -1_000, "MARKET CAFE").await;
    let (status, _) = world
        .api(
            axum::http::Method::PATCH,
            &format!("/api/v1/transactions/{chosen}"),
            Some(json!({"splits": [{"category": dining, "minor_units": -1000}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;

    let ran = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;

    assert_eq!(ran.status, StatusCode::OK);
    let outcome = ran.between(r#"id="outcome""#, "</section>");
    assert!(outcome.contains("It changed 1 transaction."));
    assert!(outcome.contains("It filed 1 transaction under Groceries."));
    assert!(outcome.contains("Nothing was marked reviewed."));
    let filed = world.read_transaction(open).await;
    assert_eq!(filed_under(&filed), Some(groceries.to_string()));
    assert_eq!(filed["review"], "unreviewed");
    assert_eq!(
        filed_under(&world.read_transaction(chosen).await),
        Some(dining.to_string()),
        "what a person chose stays"
    );
    let runs = world.events("rule.apply").await;
    assert_eq!(runs.len(), 1);
    assert!(by_member(&runs[0], world.alice.member));

    let again = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert!(again.body.contains("It changed nothing"));
}

#[tokio::test]
async fn taking_back_reopens_what_it_filled_and_leaves_what_was_reviewed() {
    let mut world = world().await;
    let groceries = world.category("Groceries").await;
    let everyday = world.account("Everyday").await;
    let open = world.transaction(everyday, -4_250, "CORNER MARKET").await;
    let reviewed = world.transaction(everyday, -1_000, "MARKET HALL").await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "market", "category": groceries}))
        .await;
    let csrf = csrf_of(&mut world.browser, &format!("/rules/{rule}")).await;
    let ran = world
        .browser
        .post_form(&format!("/rules/{rule}/apply"), &[("csrf", &csrf)])
        .await;
    assert_eq!(ran.status, StatusCode::OK);
    world.review(reviewed).await;

    let undone = world
        .browser
        .post_form(&format!("/rules/{rule}/undo"), &[("csrf", &csrf)])
        .await;

    assert_eq!(undone.status, StatusCode::OK);
    let outcome = undone.between(r#"id="outcome""#, "</section>");
    assert!(outcome.contains("It emptied 1 transaction"));
    assert!(outcome.contains("It left 1 transaction someone has reviewed"));
    let reopened = world.read_transaction(open).await;
    assert_eq!(filed_under(&reopened), None);
    assert_eq!(reopened["categorized_by"], Value::Null, "open again");
    assert_eq!(
        filed_under(&world.read_transaction(reviewed).await),
        Some(groceries.to_string())
    );
    assert_eq!(world.rules().await.len(), 1, "the rule stays");
    let undos = world.events("rule.undo").await;
    assert_eq!(undos.len(), 1);
    assert!(by_member(&undos[0], world.alice.member));
}
