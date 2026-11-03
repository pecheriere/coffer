//! How rules meet what else a transaction can be: split, reviewed, merged, linked to a
//! goal, and a category or member that can't take new work.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::TransactionId;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::CategoryKind;
use coffer_server::ledger::plan_items::PlannedItemInput;
use coffer_server::sync::apply::{AccountBatch, Coverage, apply_account_batch, apply_held};
use coffer_store::ProviderAccountUpsert;
use serde_json::json;

use super::{after, pending, record, text, world};

#[tokio::test]
async fn a_split_transaction_is_left_alone_and_a_dry_run_says_so() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;
    let food = world.category("Food").await;
    let home = world.category("Home").await;
    let bob = world.second_member().await;
    world
        .patch_transaction(
            id,
            json!({"splits": [
                {"category": food, "minor_units": -700},
                {"category": home, "minor_units": -500},
            ]}),
        )
        .await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "SHOP", "assignee": bob}))
        .await;

    let answer = world.dry_run(&rule).await;
    assert_eq!(answer["would_change"], 0);
    assert_eq!(answer["left_alone"]["split"], 1);

    assert_eq!(world.run(&rule).await["changed"], 0);
    let stored = world.stored(id).await;
    assert_eq!(stored.splits.len(), 2);
    assert_eq!(stored.assignee, None, "nobody is assigned the whole of it");
}

#[tokio::test]
async fn a_reviewed_transaction_is_left_alone() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;
    world
        .patch_transaction(id, json!({"review": "reviewed"}))
        .await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;

    let answer = world.run(&rule).await;

    assert_eq!(answer["changed"], 0);
    assert_eq!(answer["left_alone"]["reviewed"], 1);
    assert_eq!(world.stored(id).await.splits.len(), 0);
}

#[tokio::test]
async fn a_duplicate_only_a_rule_filled_merges_without_asking_and_is_recorded() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let (one, two) = (world.feed(&account).await, world.feed(&account).await);
    let target = world.arrive(&one, "a", -1200, "SHOP").await;
    world.sync(&two, vec![pending("b", -1300, "SHOP")]).await;
    let duplicate = world.holding(&two, "b").await;
    assert_ne!(duplicate, target, "two transactions to merge");
    assert_ne!(world.stored(duplicate).await.splits.len(), 0);

    let (status, merged) = world
        .call(
            Method::POST,
            &format!("/api/v1/transactions/{target}/merge"),
            &world.write,
            Some(json!({"duplicate": duplicate})),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["categorized_by"]["rule"], rule);
    assert_eq!(merged["splits"][0]["category"], groceries.to_string());
    assert_eq!(merged["splits"].as_array().unwrap().len(), 1);
    let gone = world.events("transaction.merged").await;
    let (_, splits) = gone[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "splits")
        .unwrap();
    assert_ne!(
        splits.before,
        coffer_domain::audit::AuditValue::Null,
        "what the duplicate held is in the record"
    );
}

#[tokio::test]
async fn a_duplicate_a_person_filed_still_needs_saying_so() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;
    let chosen = world.category("Chosen").await;
    let account = world.account("Everyday").await;
    let (one, two) = (world.feed(&account).await, world.feed(&account).await);
    let target = world.arrive(&one, "a", -1200, "SHOP").await;
    world.sync(&two, vec![pending("b", -1300, "SHOP")]).await;
    let duplicate = world.holding(&two, "b").await;
    world
        .patch_transaction(
            duplicate,
            json!({"splits": [{"category": chosen, "minor_units": -1300}]}),
        )
        .await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/transactions/{target}/merge"),
            &world.write,
            Some(json!({"duplicate": duplicate})),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "merge_conflict");
}

#[tokio::test]
async fn a_transfer_rule_leaves_a_goal_link_as_it_is() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let id = world.arrive(&feed, "a", -1200, "SHOP").await;
    let spending = world.category("Spending").await;
    let moving = world
        .category_of_kind("Moving", CategoryKind::Transfer)
        .await;
    let input: PlannedItemInput = serde_json::from_value(json!({
        "name": "Roof",
        "category": spending,
        "amount": {"minor_units": 500_000, "currency": "USD"},
        "direction": "outflow",
        "account_hint": account.id,
        "schedule": {"type": "goal", "start_on": "2026-11-01", "due_on": "2027-11-01"},
    }))
    .unwrap();
    let goal = world
        .ledger
        .create_planned_item(&world.alice, input, None)
        .await
        .unwrap()
        .id;
    world
        .patch_transaction(
            id,
            json!({"splits": [{"category": spending, "minor_units": -1200, "goal": goal}]}),
        )
        .await;
    let rule = world.filing("SHOP", moving).await;

    assert_eq!(world.run(&rule).await["changed"], 0);

    let stored = world.stored(id).await;
    assert_eq!(stored.splits[0].category, spending);
    assert_eq!(stored.splits[0].goal, Some(goal));

    // A rule's own split carries no goal, so filing under `transfer` breaks nothing the
    // goal rules guard: a link on it is refused as on any other.
    let other = world.arrive(&feed, "b", -50, "SHOP").await;
    let view = world.read_transaction(other).await;
    assert_eq!(view["categorized_by"]["rule"], rule);
    assert_eq!(view["splits"][0]["goal"], json!(null));
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/transactions/{other}"),
            &world.write,
            Some(json!({"splits": [{"category": moving, "minor_units": -50, "goal": goal}]})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_archived_category_stops_only_that_part_and_the_next_rule_may_fill_it() {
    let world = world().await;
    let archived = world.category("Archived").await;
    let fallback = world.category("Fallback").await;
    let bob = world.second_member().await;
    let rule = world
        .rule(json!({
            "match_kind": "contains", "pattern": "SHOP", "category": archived, "assignee": bob,
        }))
        .await;
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/categories/{archived}"),
            &world.write,
            Some(json!({"archived": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, view) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            None,
        )
        .await;
    assert_eq!(
        view["category_archived"], true,
        "the rule says why it stopped"
    );
    assert_eq!(view["category"], archived.to_string());
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let alone = world
        .stored(world.arrive(&feed, "a", -100, "SHOP").await)
        .await;
    assert_eq!(
        alone.splits.len(),
        0,
        "an archived category takes no splits"
    );
    assert_eq!(alone.assignee, Some(bob));

    let next = world.filing("SHOP", fallback).await;
    let filed = world
        .read_transaction(world.arrive(&feed, "b", -100, "SHOP").await)
        .await;
    assert_eq!(filed["splits"][0]["category"], fallback.to_string());
    assert_eq!(filed["categorized_by"]["rule"], next);
    assert_eq!(filed["assigned_by"]["rule"], rule);

    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/categories/{archived}"),
            &world.write,
            Some(json!({"archived": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let restored = world
        .stored(world.arrive(&feed, "c", -100, "SHOP").await)
        .await;
    assert_eq!(
        restored.splits[0].category, archived,
        "restored, it files again"
    );
}

#[tokio::test]
async fn an_archived_group_counts_as_an_archived_category() {
    let world = world().await;
    let category = world.category("Grouped").await;
    world.filing("SHOP", category).await;
    let (status, tree) = world
        .call(Method::GET, "/api/v1/categories", &world.write, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let group = tree["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = world
        .call(
            Method::PATCH,
            &format!("/api/v1/category-groups/{group}"),
            &world.write,
            Some(json!({"archived": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let stored = world
        .stored(world.arrive(&feed, "a", -100, "SHOP").await)
        .await;

    assert_eq!(stored.splits.len(), 0);
}

#[tokio::test]
async fn a_disabled_member_takes_no_assignment() {
    let world = world().await;
    let bob = world.second_member().await;
    let rule = world
        .rule(json!({"match_kind": "contains", "pattern": "SHOP", "assignee": bob}))
        .await;
    world
        .server
        .identity
        .disable_member(&world.alice, bob)
        .await
        .unwrap();
    let (_, view) = world
        .call(
            Method::GET,
            &format!("/api/v1/rules/{rule}"),
            &world.write,
            None,
        )
        .await;
    assert_eq!(view["assignee_disabled"], true);
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;

    let stored = world
        .stored(world.arrive(&feed, "a", -100, "SHOP").await)
        .await;

    assert_eq!(stored.assignee, None);
}

#[tokio::test]
async fn a_held_record_is_filed_when_its_account_is_bound() {
    let world = world().await;
    let groceries = world.category("Groceries").await;
    let rule = world.filing("SHOP", groceries).await;
    let account = world.account("Everyday").await;
    let feed = world.feed(&account).await;
    let other = world.account("Savings").await;
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    let unbound = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: feed.connection,
            provider_account_id: "acct-2".into(),
            name: "Savings".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: now,
        })
        .await
        .unwrap();
    apply_account_batch(
        &mut tx,
        now,
        feed.connection,
        AccountBatch {
            provider_account: unbound.clone(),
            account: None,
            transactions: vec![record("held", -100, "SHOP")],
            removed: Vec::new(),
            coverage: Coverage::Delta,
            unusable: Vec::new(),
        },
    )
    .await
    .unwrap();
    let bound = tx
        .bind_provider_account(unbound.id, other.id)
        .await
        .unwrap();
    apply_held(&mut tx, now, bound, other).await.unwrap();
    tx.commit().await.unwrap();

    let id = world.holding(&feed, "held").await;
    let events = world.events("transaction.rule_applied").await;

    assert_eq!(after(&events[0], "category_rule"), Some(text(&rule)));
    assert_eq!(events[0].entity_id, id.to_string());
}

/// A link between `outflow` and `inflow`.
async fn link(world: &super::World, outflow: TransactionId, inflow: TransactionId) {
    let (status, link) = world
        .call(
            Method::POST,
            "/api/v1/transfer-links",
            &world.write,
            Some(json!({"outflow": outflow, "inflow": inflow})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
}

#[tokio::test]
async fn a_transfer_leg_takes_only_a_transfer_category_and_may_still_be_assigned() {
    let world = world().await;
    let checking = world.account("Checking").await;
    let savings = world.account("Savings").await;
    let (from, to) = (world.feed(&checking).await, world.feed(&savings).await);
    let outflow = world.arrive(&from, "a", -5000, "MOVE TO SAVINGS").await;
    let inflow = world.arrive(&to, "b", 5000, "MOVE FROM CHECKING").await;
    link(&world, outflow, inflow).await;
    let spending = world.category("Spending").await;
    let moving = world
        .category_of_kind("Moving", CategoryKind::Transfer)
        .await;
    let bob = world.second_member().await;
    let filing = world
        .rule(json!({
            "match_kind": "contains", "pattern": "MOVE", "category": spending, "assignee": bob,
        }))
        .await;

    let tried = world.dry_run(&filing).await;
    assert_eq!(tried["would_categorize"], 0, "a movement isn't spending");
    assert_eq!(tried["would_assign"], 2);
    let answer = world.run(&filing).await;
    assert_eq!(answer["categorized"], 0);
    assert_eq!(answer["assigned"], 2);
    let stored = world.stored(outflow).await;
    assert_eq!(stored.splits.len(), 0);
    assert_eq!(stored.assignee, Some(bob));
    assert!(stored.transfer_link.is_some(), "the link stays");

    let only_filing = world.filing("MOVE", spending).await;
    let answer = world.run(&only_filing).await;
    assert_eq!(answer["left_alone"]["transfer_leg"], 2);

    let transferring = world.filing("MOVE", moving).await;
    assert_eq!(world.run(&transferring).await["categorized"], 2);
    let stored = world.stored(outflow).await;
    assert_eq!(stored.splits[0].category, moving);
    assert!(stored.transfer_link.is_some());
}

#[tokio::test]
async fn a_link_made_later_keeps_what_a_rule_already_filed() {
    let world = world().await;
    let spending = world.category("Spending").await;
    let rule = world.filing("MOVE", spending).await;
    let checking = world.account("Checking").await;
    let savings = world.account("Savings").await;
    let (from, to) = (world.feed(&checking).await, world.feed(&savings).await);
    let outflow = world.arrive(&from, "a", -5000, "MOVE TO SAVINGS").await;
    let inflow = world.arrive(&to, "b", 5000, "MOVE FROM CHECKING").await;

    link(&world, outflow, inflow).await;

    let view = world.read_transaction(outflow).await;
    assert_eq!(view["splits"][0]["category"], spending.to_string());
    assert_eq!(view["categorized_by"]["rule"], rule);
}

#[tokio::test]
async fn unmerging_runs_no_rule() {
    let world = world().await;
    let account = world.account("Everyday").await;
    let (one, two) = (world.feed(&account).await, world.feed(&account).await);
    let target = world.arrive(&one, "a", -1200, "SHOP").await;
    let duplicate = world.arrive(&two, "b", -1300, "SHOP").await;
    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/transactions/{target}/merge"),
            &world.write,
            Some(json!({"duplicate": duplicate})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let groceries = world.category("Groceries").await;
    world.filing("SHOP", groceries).await;
    let source = world.stored(target).await.sources[1].id;

    let (status, split_off) = world
        .call(
            Method::POST,
            &format!("/api/v1/transactions/{target}/unmerge"),
            &world.write,
            Some(json!({"source": source})),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{split_off}");
    assert_eq!(split_off["splits"], json!([]));
}
