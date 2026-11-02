//! What a split may name, and what a link stops a goal doing.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::AuditValue;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::transactions::CategoryKind;
use serde_json::{Value, json};

use super::super::{goal, item_uri, one_off, recurring, with, world};
use super::{HAS_LINKS, NO_SUCH_GOAL, TRANSACTIONS, euros, linked, transaction_uri};

#[tokio::test]
async fn split_links_to_a_goal_and_reads_back() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;

    let splits = linked(world.expense, -500, &goal_id);
    let (status, answer) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(answer["splits"][0]["goal"], goal_id.as_str());
    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn unlinked_split_answers_a_null_goal() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let splits = json!([{"category": world.expense, "minor_units": -500}]);
    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;
    assert_eq!(status, StatusCode::OK);

    let read = world.read_transaction(&transaction).await;

    assert!(read["splits"][0].get("goal").is_some_and(Value::is_null));
}

#[tokio::test]
async fn link_is_audited_in_the_splits_change() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;

    world.link(&transaction, -500, &goal_id).await;

    let expected = format!(
        r#"[{{"category":"{}","minor_units":-500,"goal":"{goal_id}"}}]"#,
        world.expense
    );
    assert_eq!(
        world.audited_after(&transaction, "splits").await,
        AuditValue::Text(expected)
    );
}

#[tokio::test]
async fn unlinked_splits_keep_their_audit_text() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let splits = json!([{"category": world.expense, "minor_units": -500}]);

    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let expected = format!(r#"[{{"category":"{}","minor_units":-500}}]"#, world.expense);
    assert_eq!(
        world.audited_after(&transaction, "splits").await,
        AuditValue::Text(expected)
    );
}

#[tokio::test]
async fn non_goal_item_refused() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;

    for item in [recurring(world.expense), one_off(world.expense)] {
        let item = world.create(&item).await;
        let splits = linked(world.expense, -500, &item);
        let (status, answer) = world.set_splits(&world.admin, &transaction, splits).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(answer.contains("a split names a planned item that is not a goal"));
    }
}

#[tokio::test]
async fn other_currency_refused() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let item = world
        .create(&with(goal(world.expense), "amount", euros(300_000)))
        .await;

    let splits = linked(world.expense, -500, &item);
    let (status, answer) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(answer.contains("a split's goal must be in the transaction's currency"));
}

#[tokio::test]
async fn completed_goal_may_be_linked() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let completed = with(
        goal(world.expense),
        "schedule",
        json!({"type": "goal", "start_on": "2027-01-01", "due_on": "2027-07-01",
               "completed": true}),
    );
    let item = world.create(&completed).await;

    let splits = linked(world.expense, -500, &item);
    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn inactive_goal_may_be_linked() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let item = world
        .create(&with(goal(world.expense), "active", json!(false)))
        .await;

    let splits = linked(world.expense, -500, &item);
    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn batch_refuses_by_operation_index() {
    let world = world().await;
    let home = world.account("Home").await;
    let (first, second) = (
        world.manual(home, -500).await,
        world.manual(home, -700).await,
    );
    let goal_id = world.create(&goal(world.expense)).await;
    let body = json!({"operations": [
        {"id": first, "patch": {"splits": linked(world.expense, -500, &goal_id)}},
        {"id": second, "patch": {"splits": linked(world.expense, -700, PlannedItemId::new())}},
    ]});

    let uri = format!("{TRANSACTIONS}/batch");
    let (status, answer) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(answer["detail"], format!("operations[1]: {NO_SUCH_GOAL}"));
    let untouched = world.read_transaction(&first).await;
    assert_eq!(untouched["splits"], json!([]));
}

#[tokio::test]
async fn kept_visible_link_is_held_to_the_currency_rule() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let item = world
        .create(&with(goal(world.expense), "amount", euros(300_000)))
        .await;
    world.force_link(&transaction, -500, &item).await;
    let (_, other) = world.category(CategoryKind::Expense).await;

    let splits = linked(other, -500, &item);
    let (status, answer) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(answer.contains("a split's goal must be in the transaction's currency"));
}

#[tokio::test]
async fn goal_with_links_cannot_be_deleted() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -500, &goal_id).await;

    let (status, answer) = world.delete_item(&goal_id).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["code"], "conflict");
    assert_eq!(answer["detail"], HAS_LINKS);
    let uri = item_uri(&goal_id);
    let (still_there, _) = world.call(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(still_there, StatusCode::OK);
}

#[tokio::test]
async fn goal_with_links_cannot_change_currency() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -500, &goal_id).await;

    let body = json!({"amount": euros(300_000)});
    world
        .patch_refused(&goal_id, &body, StatusCode::CONFLICT)
        .await;

    let uri = item_uri(&goal_id);
    let (_, read) = world.call(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(read["amount"]["currency"], "USD");
}

#[tokio::test]
async fn goal_with_links_may_change_its_amount() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -500, &goal_id).await;

    let body = json!({"amount": {"minor_units": 400_000, "currency": "USD"}});
    let read = world.patch(&goal_id, &body).await;

    assert_eq!(read["amount"]["minor_units"], 400_000);
}

#[tokio::test]
async fn goal_without_links_may_change_currency() {
    let world = world().await;
    let goal_id = world.create(&goal(world.expense)).await;

    let read = world
        .patch(&goal_id, &json!({"amount": euros(300_000)}))
        .await;

    assert_eq!(read["amount"]["currency"], "EUR");
}

#[tokio::test]
async fn deleting_a_transaction_frees_the_goal() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -500, &goal_id).await;

    let uri = transaction_uri(&transaction);
    let (status, _) = world.call(Method::DELETE, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(world.delete_item(&goal_id).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn removing_the_link_frees_the_goal() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -500, &goal_id).await;

    let splits = json!([{"category": world.expense, "minor_units": -500}]);
    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(world.delete_item(&goal_id).await.0, StatusCode::NO_CONTENT);
}

/// The gate on `splits != before`: what isn't changing isn't checked again.
#[tokio::test]
async fn resending_a_wrong_currency_link_unchanged_is_no_change() {
    let world = world().await;
    let transaction = world.manual(world.account("Home").await, -500).await;
    let item = world
        .create(&with(goal(world.expense), "amount", euros(300_000)))
        .await;
    world.force_link(&transaction, -500, &item).await;
    let audited = world.transaction_audit(&transaction).await.len();

    let splits = linked(world.expense, -500, &item);
    let (status, _) = world.set_splits(&world.admin, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.transaction_audit(&transaction).await.len(), audited);
}
