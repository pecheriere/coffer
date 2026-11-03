//! Finding and taking back what a rule did.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{CategoryId, MemberId, TransactionId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use serde_json::json;

use super::{Feed, World, after, world};

struct Filed {
    feed: Feed,
    id: TransactionId,
    rule: String,
    wrong: CategoryId,
    bob: MemberId,
}

/// A transaction a rule filed and assigned on arrival.
async fn filed(world: &World) -> Filed {
    let wrong = world.category("Wrong").await;
    let bob = world.second_member().await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": wrong, "assignee": bob,
        }))
        .await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;
    Filed {
        feed,
        id,
        rule,
        wrong,
        bob,
    }
}

async fn undo(world: &World, rule: &str) -> (StatusCode, serde_json::Value) {
    world
        .call(
            Method::POST,
            &format!("/api/v1/rules/{rule}/undo"),
            &world.write,
            None,
        )
        .await
}

#[tokio::test]
async fn what_a_rule_filled_can_be_listed() {
    let world = world().await;
    let filed = filed(&world).await;
    let other = world.category("Other").await;
    world
        .sync(&filed.feed, vec![super::record("b", -5, "SHOP")])
        .await;
    let edited = world.holding(&filed.feed, "b").await;
    world
        .patch_transaction(
            edited,
            json!({"splits": [{"category": other, "minor_units": -5}], "assignee": null}),
        )
        .await;
    let uri = format!("/api/v1/transactions?rule={}", filed.rule);

    let (status, list) = world.call(Method::GET, &uri, &world.write, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], filed.id.to_string());
}

#[tokio::test]
async fn undoing_reopens_each_field_so_a_corrected_rule_can_fill_it() {
    let world = world().await;
    let filed = filed(&world).await;

    let (status, answer) = undo(&world, &filed.rule).await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["changed"], 1);
    assert_eq!(answer["categorized"], 1);
    assert_eq!(answer["assigned"], 1);
    let view = world.read_transaction(filed.id).await;
    assert_eq!(view["splits"], json!([]));
    assert_eq!(view["assignee"], json!(null));
    assert_eq!(view["categorized_by"], json!(null), "nobody has chosen");
    assert_eq!(view["assigned_by"], json!(null), "nobody has chosen");

    let taken = world.events("transaction.rule_undone").await;
    assert_eq!(taken[0].entity_id, filed.id.to_string());
    assert!(matches!(taken[0].actor, AuditActor::Member { .. }));
    let (_, before) = taken[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "category")
        .unwrap();
    assert_eq!(before.before, AuditValue::Text(filed.wrong.to_string()));
    let batch = world.events("rule.undo").await;
    assert_eq!(after(&batch[0], "transactions"), Some(AuditValue::Int(1)));

    // Correct the rule, and it fills what it had got wrong.
    let right = world.category("Right").await;
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/rules/{}", filed.rule),
            &world.write,
            Some(json!({"category": right})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.run(&filed.rule).await["changed"], 1);
    let stored = world.stored(filed.id).await;
    assert_eq!(stored.splits[0].category, right);
    assert_eq!(stored.assignee, Some(filed.bob));
}

#[tokio::test]
async fn clearing_by_hand_still_stays_cleared_unlike_undoing() {
    let world = world().await;
    let filed = filed(&world).await;
    world
        .patch_transaction(filed.id, json!({"splits": [], "assignee": null}))
        .await;

    let (_, answer) = undo(&world, &filed.rule).await;

    assert_eq!(answer["changed"], 0, "that's a person's, not the rule's");
    assert_eq!(world.run(&filed.rule).await["changed"], 0);
    assert_ne!(
        world.read_transaction(filed.id).await["categorized_by"],
        json!(null)
    );
}

#[tokio::test]
async fn undoing_leaves_what_a_person_changed_since_and_what_they_reviewed() {
    let world = world().await;
    let filed = filed(&world).await;
    let chosen = world.category("Chosen").await;
    world
        .patch_transaction(
            filed.id,
            json!({"splits": [{"category": chosen, "minor_units": -1200}]}),
        )
        .await;
    world
        .sync(&filed.feed, vec![super::record("b", -5, "SHOP")])
        .await;
    let reviewed = world.holding(&filed.feed, "b").await;
    world
        .patch_transaction(reviewed, json!({"review": "reviewed"}))
        .await;

    let (_, answer) = undo(&world, &filed.rule).await;

    assert_eq!(answer["changed"], 1);
    assert_eq!(answer["categorized"], 0, "the category is a person's now");
    assert_eq!(answer["assigned"], 1, "the assignee is still the rule's");
    assert_eq!(answer["reviewed"], 1);
    assert_eq!(world.stored(filed.id).await.splits[0].category, chosen);
    assert_eq!(world.stored(filed.id).await.assignee, None);
    let stored = world.stored(reviewed).await;
    assert_eq!(stored.splits[0].category, filed.wrong);
    assert_eq!(stored.assignee, Some(filed.bob));
}

#[tokio::test]
async fn a_deleted_rule_can_be_undone_with_write_on_every_account() {
    let world = world().await;
    let filed = filed(&world).await;
    let narrow = world
        .token(
            Scope::Write,
            vec![TokenGrant {
                target: GrantTarget::Account(filed.feed.account.id),
                level: AccountLevel::Write,
            }],
        )
        .await;
    let (status, _) = world
        .call(
            Method::DELETE,
            &format!("/api/v1/rules/{}", filed.rule),
            &world.write,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let uri = format!("/api/v1/rules/{}/undo", filed.rule);

    let (status, _) = world.call(Method::POST, &uri, &narrow, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(world.stored(filed.id).await.splits.len(), 1);

    let (status, answer) = undo(&world, &filed.rule).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["changed"], 1);
}

#[tokio::test]
async fn undoing_is_held_to_the_same_rights_as_running() {
    let world = world().await;
    let mine = world.account("Mine").await;
    let theirs = world.account("Theirs").await;
    let (my_feed, their_feed) = (world.feed(&mine).await, world.feed(&theirs).await);
    let category = world.category("Groceries").await;
    let everywhere = world.filing("SHOP", category).await;
    let my_id = world.arrive(&my_feed, "a", -1, "SHOP").await;
    let their_id = world.arrive(&their_feed, "b", -1, "SHOP").await;
    let write_mine = TokenGrant {
        target: GrantTarget::Account(mine.id),
        level: AccountLevel::Write,
    };
    let narrow = world.token(Scope::Write, vec![write_mine]).await;
    let read = world.token(Scope::Read, Vec::new()).await;
    let uri = format!("/api/v1/rules/{everywhere}/undo");

    for token in [&narrow, &read] {
        let (status, _) = world.call(Method::POST, &uri, token, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(world.stored(my_id).await.splits.len(), 1);

    // A token that may list sees only what it can read.
    let (_, list) = world
        .call(
            Method::GET,
            &format!("/api/v1/transactions?rule={everywhere}"),
            &narrow,
            None,
        )
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], my_id.to_string());

    // One moved to its account: it takes back only there, though the rule filled both.
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/rules/{everywhere}"),
            &world.write,
            Some(json!({"account": mine.id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, answer) = world.call(Method::POST, &uri, &narrow, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["changed"], 1);
    assert_eq!(world.stored(my_id).await.splits.len(), 0);
    assert_eq!(world.stored(their_id).await.splits.len(), 1);
}
