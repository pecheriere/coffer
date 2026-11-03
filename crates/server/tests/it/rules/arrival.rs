//! What rules do to a transaction as it first arrives from a sync.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::AuditActor;
use coffer_domain::transactions::{ReviewStatus, TransactionFlag};
use serde_json::json;

use super::{after, pending, record, text, world};

#[tokio::test]
async fn a_new_transaction_is_filed_and_assigned_and_says_by_which_rule() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let bob = world.second_member().await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "corner shop",
            "category": groceries, "assignee": bob,
        }))
        .await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let id = world.arrive(&feed, "a", -1200, "THE CORNER SHOP 42").await;

    let view = world.read_transaction(id).await;
    assert_eq!(view["splits"][0]["category"], groceries.to_string());
    assert_eq!(view["splits"][0]["amount"]["minor_units"], -1200);
    assert_eq!(view["splits"][0]["goal"], json!(null));
    assert_eq!(view["assignee"], bob.to_string());
    assert_eq!(view["categorized_by"]["rule"], rule);
    assert_eq!(view["categorized_by"]["system"], "system:rules");
    assert_eq!(view["categorized_by"]["member"], json!(null));
    assert_eq!(view["assigned_by"]["rule"], rule);
    assert_eq!(view["assigned_by"]["system"], "system:rules");

    let events = world.events("transaction.rule_applied").await;
    let [event] = events.as_slice() else {
        panic!("one entry for the transaction");
    };
    assert_eq!(event.entity_id, id.to_string());
    assert_eq!(
        event.actor,
        AuditActor::System {
            reason: "system:rules".into()
        }
    );
    assert_eq!(after(event, "category"), Some(text(&groceries)));
    assert_eq!(after(event, "category_rule"), Some(text(&rule)));
    assert_eq!(after(event, "assignee"), Some(text(&bob)));
    assert_eq!(after(event, "assignee_rule"), Some(text(&rule)));
}

#[tokio::test]
async fn a_rule_never_makes_a_transaction_reviewed() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let id = world.arrive(&feed, "a", -1200, "SHOP").await;

    let stored = world.stored(id).await;
    assert_eq!(stored.review, ReviewStatus::Unreviewed);
    assert_eq!(stored.reviewed, None);
}

#[tokio::test]
async fn the_order_decides_each_field_and_position_ties_go_to_the_oldest() {
    let world = world().await;
    let first = world.category("First").await;
    let second = world.category("Second").await;
    let third = world.category("Third").await;
    let bob = world.second_member().await;
    // Created in the opposite order to the order they run in.
    world
        .rule(
            json!({"match_kind": "contains", "pattern": "SHOP", "category": third, "position": 9}),
        )
        .await;
    let assigning = world
        .rule(json!({"match_kind": "contains", "pattern": "SHOP", "assignee": bob, "position": 5}))
        .await;
    let oldest = world
        .rule(
            json!({"match_kind": "contains", "pattern": "SHOP", "category": first, "position": 5}),
        )
        .await;
    world
        .rule(
            json!({"match_kind": "contains", "pattern": "SHOP", "category": second, "position": 5}),
        )
        .await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let view = world
        .read_transaction(world.arrive(&feed, "a", -1200, "SHOP").await)
        .await;

    assert_eq!(view["splits"][0]["category"], first.to_string());
    assert_eq!(view["categorized_by"]["rule"], oldest);
    assert_eq!(view["assigned_by"]["rule"], assigning);
}

#[tokio::test]
async fn a_rule_that_is_off_or_does_not_match_does_nothing() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "enabled": false,
        }))
        .await;
    world
        .rule(json!({"match_kind": "starts_with", "pattern": "SHOP", "category": groceries}))
        .await;
    world
        .rule(json!({
            "match_kind": "contains", "pattern": "CORNER", "category": groceries,
            "amount": {"currency": "USD", "min_minor": 1},
        }))
        .await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let id = world.arrive(&feed, "a", -1200, "CORNER SHOP").await;

    let stored = world.stored(id).await;
    assert_eq!(stored.splits.len(), 0);
    assert_eq!(stored.categorized, None);
    assert_eq!(world.events("transaction.rule_applied").await.len(), 0);
}

#[tokio::test]
async fn hostile_bank_text_is_only_text() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;
    let wildcard = world.category("Wildcard").await;
    world.filing("%", wildcard).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let hostile = "'; DROP TABLE rules; -- <script>alert(1)</script> ((((a+)+)+)+$ \u{0}\n SHOP";
    let id = world.arrive(&feed, "a", -1200, hostile).await;

    let (status, list) = world
        .call(Method::GET, "/api/v1/rules", &world.write, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let view = world.read_transaction(id).await;
    assert_eq!(
        view["splits"][0]["category"],
        groceries.to_string(),
        "% matched nothing, since it is no wildcard"
    );
}

#[tokio::test]
async fn only_first_arrival_runs_rules_not_a_record_seen_again() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;

    world.sync(&feed, vec![record("a", -1200, "SHOP")]).await;
    world
        .sync(&feed, vec![record("a", -1300, "SHOP AGAIN")])
        .await;

    assert_eq!(
        world.stored(id).await.splits.len(),
        0,
        "history changes only when someone runs the rule"
    );
}

#[tokio::test]
async fn posting_keeps_what_a_rule_chose_and_its_name_and_runs_no_rule() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("PENDING SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    world
        .sync(&feed, vec![pending("a", -1200, "PENDING SHOP")])
        .await;
    let id = world.holding(&feed, "a").await;

    // It posts under another description, with a tip, and no longer matches.
    world
        .sync(&feed, vec![record("a", -1500, "POSTED GROCER")])
        .await;

    let view = world.read_transaction(id).await;
    assert_eq!(view["pending"], false);
    assert_eq!(view["splits"][0]["category"], groceries.to_string());
    assert_eq!(view["splits"][0]["amount"]["minor_units"], -1500);
    assert_eq!(view["categorized_by"]["rule"], rule);
    assert_eq!(view["categorized_by"]["system"], "system:sync");

    // One that didn't match while pending stays empty when it posts as a match.
    world
        .sync(&feed, vec![pending("b", -700, "AUTH HOLD")])
        .await;
    let other = world.holding(&feed, "b").await;
    world
        .sync(&feed, vec![record("b", -700, "PENDING SHOP")])
        .await;
    assert_eq!(world.stored(other).await.splits.len(), 0);
    assert_eq!(world.dry_run(&rule).await["would_change"], 1);
}

#[tokio::test]
async fn a_pending_record_taken_back_goes_with_what_a_rule_filled() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    world.sync(&feed, vec![pending("a", -1200, "SHOP")]).await;
    let id = world.holding(&feed, "a").await;

    world
        .sync_removing(&feed, Vec::new(), vec!["a".to_owned()])
        .await;

    assert!(
        world.server.store.get_transaction(id).await.is_err(),
        "a rule's work is no reason to keep a transaction whose bank took it back"
    );
}

#[tokio::test]
async fn a_pending_record_taken_back_stays_once_a_person_has_filed_it() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let other = world.category("Other").await;
    world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    world.sync(&feed, vec![pending("a", -1200, "SHOP")]).await;
    let id = world.holding(&feed, "a").await;
    world
        .patch_transaction(
            id,
            json!({"splits": [{"category": other, "minor_units": -1200}]}),
        )
        .await;

    world
        .sync_removing(&feed, Vec::new(), vec!["a".to_owned()])
        .await;

    let stored = world.stored(id).await;
    assert!(stored.flags.contains(&TransactionFlag::SourceRemoved));
    assert_eq!(stored.splits[0].category, other);
}

#[tokio::test]
async fn a_zero_amount_arrives_assigned_but_not_filed() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let bob = world.second_member().await;
    world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "assignee": bob,
        }))
        .await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let stored = world
        .stored(world.arrive(&feed, "a", 0, "SHOP").await)
        .await;

    assert_eq!(stored.splits.len(), 0);
    assert_eq!(stored.assignee, Some(bob));
}
