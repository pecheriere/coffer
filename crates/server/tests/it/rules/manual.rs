//! Trying a rule on the transactions held, and running it by hand.

use axum::http::{Method, StatusCode};
use coffer_domain::transactions::ReviewStatus;
use serde_json::json;

use super::{Feed, World, after, record, text, world};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::ids::TransactionId;

async fn held(world: &World, description: &str) -> (Feed, TransactionId) {
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, description).await;
    (feed, id)
}

#[tokio::test]
async fn a_dry_run_says_what_a_run_does_and_writes_nothing() {
    let world = world().await;
    let (feed, first) = held(&world, "CORNER SHOP").await;
    world.sync(&feed, vec![record("b", -300, "SHOP TWO")]).await;
    world.sync(&feed, vec![record("c", -300, "OTHER")]).await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("shop", groceries).await;
    let before = world.events("transaction.rule_applied").await.len();

    let answer = world.dry_run(&rule).await;

    assert_eq!(answer["would_change"], 2);
    assert_eq!(answer["would_categorize"], 2);
    assert_eq!(answer["would_assign"], 0);
    assert_eq!(answer["changes"][0]["transaction"], first.to_string());
    assert_eq!(answer["changes"][0]["description"], "CORNER SHOP");
    assert_eq!(answer["changes"][0]["category"], groceries.to_string());
    assert_eq!(answer["changes"][0]["amount"]["minor_units"], -1200);
    assert_eq!(world.stored(first).await.splits.len(), 0);
    assert_eq!(
        world.events("transaction.rule_applied").await.len(),
        before,
        "nothing was written"
    );

    let (status, limited) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}/dry-run?limit=1"),
            &world.write,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(limited["changes"].as_array().unwrap().len(), 1);
    assert_eq!(limited["would_change"], 2, "the count covers them all");
}

#[tokio::test]
async fn a_run_fills_what_a_dry_run_said_names_the_runner_and_the_rule_and_is_audited() {
    let world = world().await;
    let (_feed, id) = held(&world, "CORNER SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    assert_eq!(world.dry_run(&rule).await["would_change"], 1);

    let answer = world.run(&rule).await;

    assert_eq!(answer["changed"], 1);
    assert_eq!(answer["categorized"], 1);
    assert_eq!(answer["assigned"], 0);
    let view = world.read_transaction(id).await;
    assert_eq!(view["splits"][0]["category"], groceries.to_string());
    assert_eq!(view["categorized_by"]["rule"], rule);
    assert_eq!(
        view["categorized_by"]["member"],
        world.alice.member.to_string()
    );
    assert_eq!(view["categorized_by"]["system"], json!(null));
    assert_eq!(view["review"], "unreviewed");

    let applied = world.events("rule.apply").await;
    let [applied] = applied.as_slice() else {
        panic!("one run, one batch");
    };
    assert_eq!(applied.entity_id, rule);
    assert!(matches!(applied.actor, AuditActor::Member { .. }));
    assert_eq!(after(applied, "transactions"), Some(AuditValue::Int(1)));
    assert_eq!(after(applied, "categorized"), Some(AuditValue::Int(1)));
    let filled = world.events("transaction.rule_applied").await;
    assert_eq!(after(&filled[0], "category_rule"), Some(text(&rule)));

    // Running it again finds nothing to do and records nothing.
    let again = world.run(&rule).await;
    assert_eq!(again["changed"], 0);
    assert_eq!(world.events("rule.apply").await.len(), 1);
    assert_eq!(world.dry_run(&rule).await["would_change"], 0);
}

#[tokio::test]
async fn a_run_only_fills_empties_and_a_person_s_edit_stands() {
    let world = world().await;
    let (feed, cleared) = held(&world, "SHOP").await;
    world.sync(&feed, vec![record("b", -300, "SHOP")]).await;
    let filed = world.holding(&feed, "b").await;
    world.sync(&feed, vec![record("c", -300, "SHOP")]).await;
    let open = world.holding(&feed, "c").await;
    let chosen = world.category("Chosen").await;
    let groceries = world.category("Groceries").await;
    let alice = world.alice.member;
    let bob = world.second_member().await;
    // A person filed and then cleared this one, and assigned and unassigned it.
    world
        .patch_transaction(
            cleared,
            json!({"splits": [{"category": chosen, "minor_units": -1200}], "assignee": alice}),
        )
        .await;
    world
        .patch_transaction(cleared, json!({"splits": [], "assignee": null}))
        .await;
    world
        .patch_transaction(
            filed,
            json!({"splits": [{"category": chosen, "minor_units": -300}]}),
        )
        .await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "assignee": bob,
        }))
        .await;

    let answer = world.run(&rule).await;

    assert_eq!(answer["changed"], 2);
    assert_eq!(answer["categorized"], 1);
    assert_eq!(answer["assigned"], 2);
    let stored = world.stored(cleared).await;
    assert_eq!(stored.splits.len(), 0, "the cleared category stays empty");
    assert_eq!(stored.assignee, None, "the cleared assignee stays empty");
    let stored = world.stored(filed).await;
    assert_eq!(stored.splits[0].category, chosen);
    assert_eq!(stored.assignee, Some(bob), "only the empty field is filled");
    let stored = world.stored(open).await;
    assert_eq!(stored.splits[0].category, groceries);
    assert_eq!(stored.assignee, Some(bob));
}

#[tokio::test]
async fn undoing_a_rule_is_an_ordinary_edit_that_stays_undone() {
    let world = world().await;
    let (_feed, id) = held(&world, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let bob = world.second_member().await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "assignee": bob,
        }))
        .await;
    world.run(&rule).await;

    let view = world
        .patch_transaction(id, json!({"splits": [], "assignee": null}))
        .await;

    assert_eq!(view["categorized_by"]["rule"], json!(null));
    assert_eq!(view["assigned_by"]["rule"], json!(null));
    assert_eq!(view["assignee"], json!(null));
    assert_eq!(world.run(&rule).await["changed"], 0, "the edit stands");
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn deleting_a_rule_leaves_what_it_did_and_who_did_it() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;

    let (status, _) = world
        .call(
            Method::DELETE,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let view = world.read_transaction(id).await;
    assert_eq!(view["splits"][0]["category"], groceries.to_string());
    assert_eq!(view["categorized_by"]["rule"], rule);
    let gone = world.events("rule.delete").await;
    assert_eq!(gone[0].entity_id, rule);
    let (status, _) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    world.sync(&feed, vec![record("b", -9, "SHOP")]).await;
    let later = world.holding(&feed, "b").await;
    assert_eq!(world.stored(later).await.splits.len(), 0);
}

#[tokio::test]
async fn changing_a_rule_changes_nothing_it_already_did() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let other = world.category("Other").await;
    let rule = world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;

    let (status, view) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            Some(json!({"category": other})),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["category"], other.to_string());
    assert_eq!(world.stored(id).await.splits[0].category, groceries);
    let updates = world.events("rule.update").await;
    assert_eq!(after(&updates[0], "category"), Some(text(&other)));
}

#[tokio::test]
async fn running_rules_one_at_a_time_keeps_their_order() {
    let world = world().await;
    let (_feed, id) = held(&world, "SHOP").await;
    let first = world.category("First").await;
    let second = world.category("Second").await;
    let earlier = world.filing("SHOP", first).await;
    let later = world.filing("SHOP", second).await;

    // The later one, run first, leaves the field to the one that would win.
    let answer = world.run(&later).await;
    assert_eq!(answer["changed"], 0);
    assert_eq!(answer["left_alone"]["earlier_rule"], 1);
    assert_eq!(world.stored(id).await.splits.len(), 0);

    assert_eq!(world.run(&earlier).await["changed"], 1);
    assert_eq!(world.stored(id).await.splits[0].category, first);
}

#[tokio::test]
async fn a_rule_that_is_off_can_be_tried_and_run_by_hand() {
    let world = world().await;
    let (_feed, id) = held(&world, "SHOP").await;
    let groceries = world.category("Groceries").await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": groceries, "enabled": false,
        }))
        .await;

    assert_eq!(world.dry_run(&rule).await["would_change"], 1);
    assert_eq!(world.run(&rule).await["changed"], 1);

    assert_eq!(world.stored(id).await.splits[0].category, groceries);
}

#[tokio::test]
async fn a_manual_transaction_is_filled_only_by_a_run() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;

    let (status, created) = world
        .call(
            Method::POST,
            "/api/v1/transactions",
            &world.write,
            Some(json!({
                "account": account.id, "posted_on": "2026-10-10", "minor_units": -100,
                "description": "SHOP",
            })),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["splits"], json!([]));
    assert_eq!(world.run(&rule).await["changed"], 1);
}

#[tokio::test]
async fn a_long_run_commits_in_several_short_batches_each_with_its_count() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let records = (0..250)
        .map(|n| record(&format!("r{n}"), -100 - n, "SHOP"))
        .collect();
    world.sync(&feed, records).await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;

    assert_eq!(world.run(&rule).await["changed"], 250);

    let batches = world.events("rule.apply").await;
    assert_eq!(batches.len(), 3);
    let counts: Vec<_> = batches
        .iter()
        .map(|batch| after(batch, "transactions"))
        .collect();
    assert_eq!(
        counts,
        [
            Some(AuditValue::Int(100)),
            Some(AuditValue::Int(100)),
            Some(AuditValue::Int(50))
        ]
    );
    assert_eq!(world.dry_run(&rule).await["would_change"], 0);
    let stored = world.holding(&feed, "r7").await;
    assert_eq!(world.stored(stored).await.review, ReviewStatus::Unreviewed);
}
