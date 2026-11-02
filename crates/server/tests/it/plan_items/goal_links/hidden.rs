//! A goal the token can't see: new to a transaction it answers like a missing
//! one, already linked it may be kept, and every change to it is audited.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::transactions::CategoryKind;
use serde_json::{Value, json};

use super::super::{World, goal, item_uri, on, one_off, with, world};
use super::{HiddenLink, NO_SUCH_GOAL, TRANSACTIONS, euros, linked, transaction_uri};

impl World {
    /// A goal hinted at a ledger of its own, which no token here is granted.
    async fn hidden_goal(&self) -> String {
        let hidden = self.account("Hidden").await;
        self.create(&with(goal(self.expense), "account_hint", json!(hidden)))
            .await
    }

    /// The `before` and `after` of the `splits` change in `link`'s latest
    /// audited update, which must be the token's.
    async fn audited_splits(&self, link: &HiddenLink) -> (AuditValue, AuditValue) {
        let events = self.transaction_audit(&link.transaction).await;
        let event = events
            .iter()
            .find(|event| event.action == "transaction.update")
            .unwrap();
        assert_eq!(
            event.actor,
            AuditActor::Member {
                member: self.alice.member,
                agent: Some(link.token_id),
            }
        );
        let (_, change) = event
            .changes
            .iter()
            .find(|(name, _)| *name == "splits")
            .unwrap();
        (change.before.clone(), change.after.clone())
    }
}

#[tokio::test]
async fn missing_and_hidden_goal_answer_alike() {
    let world = world().await;
    let (home, hidden, glimpsed) = (
        world.account("Home").await,
        world.account("Hidden").await,
        world.account("Glimpsed").await,
    );
    let on_hidden = |body| with(body, "account_hint", json!(hidden));
    let answers_of_hidden = [
        world.create(&on_hidden(goal(world.expense))).await,
        world
            .create(&with(goal(world.expense), "account_hint", json!(glimpsed)))
            .await,
        // Whatever else a hidden item would be refused for stays unsaid.
        world.create(&on_hidden(one_off(world.expense))).await,
        world
            .create(&with(
                on_hidden(goal(world.expense)),
                "amount",
                euros(300_000),
            ))
            .await,
    ];
    let (_, token) = world
        .token(Scope::Write, vec![on(home, Write), on(glimpsed, Balances)])
        .await;
    let transaction = world.manual(home, -500).await;
    let missing = linked(world.expense, -500, PlannedItemId::new());
    let missing = world.set_splits(&token, &transaction, missing).await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(missing.1.contains(NO_SUCH_GOAL));

    for item in answers_of_hidden {
        let splits = linked(world.expense, -500, &item);
        let answer = world.set_splits(&token, &transaction, splits).await;
        assert!(
            answer == missing,
            "a hidden goal answers like a missing one"
        );
    }
}

#[tokio::test]
async fn goal_hinted_at_a_readable_ledger_may_be_linked() {
    let world = world().await;
    let (home, readable) = (world.account("Home").await, world.account("Readable").await);
    let goal_id = world
        .create(&with(goal(world.expense), "account_hint", json!(readable)))
        .await;
    let (_, token) = world
        .token(
            Scope::Write,
            vec![on(home, Write), on(readable, Transactions)],
        )
        .await;
    let transaction = world.manual(home, -500).await;

    let splits = linked(world.expense, -500, &goal_id);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn read_with_hidden_goal_shows_only_its_opaque_id() {
    let world = world().await;
    let (home, hidden) = (world.account("Home").await, world.account("Hidden").await);
    let goal_id = world
        .create(&with(goal(world.expense), "account_hint", json!(hidden)))
        .await;
    let transaction = world.manual(home, -500).await;
    world.link(&transaction, -500, &goal_id).await;
    let (_, token) = world.token(Scope::Read, vec![on(home, Transactions)]).await;

    let uri = transaction_uri(&transaction);
    let (status, read) = world.send(Method::GET, &uri, &token, None, None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(read.contains(&goal_id));
    assert!(!read.contains("Summer trip"));
    let (status, _) = world
        .send(Method::GET, &item_uri(&goal_id), &token, None, None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unchanged_link_to_a_hidden_goal_is_no_change() {
    let world = world().await;
    let (home, hidden) = (world.account("Home").await, world.account("Hidden").await);
    let goal_id = world
        .create(&with(goal(world.expense), "account_hint", json!(hidden)))
        .await;
    let transaction = world.manual(home, -500).await;
    world.link(&transaction, -500, &goal_id).await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;
    let audited = world.transaction_audit(&transaction).await.len();

    let splits = linked(world.expense, -500, &goal_id);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.transaction_audit(&transaction).await.len(), audited);
}

#[tokio::test]
async fn kept_link_to_a_hidden_goal_survives_a_changed_split() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let (_, other) = world.category(CategoryKind::Expense).await;

    let splits = linked(other, -500, &link.goal_id);
    let (status, _) = world
        .set_splits(&link.token, &link.transaction, splits)
        .await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&link.transaction).await;
    assert_eq!(read["splits"][0]["category"], json!(other));
    assert_eq!(read["splits"][0]["goal"], link.goal_id.as_str());
}

#[tokio::test]
async fn moving_a_hidden_goal_to_another_transaction_answers_like_a_missing_goal() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let elsewhere = world.manual(link.home, -500).await;
    let missing = linked(world.expense, -500, PlannedItemId::new());
    let missing = world.set_splits(&link.token, &elsewhere, missing).await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let splits = linked(world.expense, -500, &link.goal_id);
    let answer = world.set_splits(&link.token, &elsewhere, splits).await;

    assert!(
        answer == missing,
        "a hidden goal answers like a missing one"
    );
}

#[tokio::test]
async fn a_kept_link_opens_no_other_hidden_goal() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let another = world
        .create(&with(
            goal(world.expense),
            "account_hint",
            json!(world.account("Also hidden").await),
        ))
        .await;
    let missing = linked(world.expense, -500, PlannedItemId::new());
    let missing = world
        .set_splits(&link.token, &link.transaction, missing)
        .await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let splits = linked(world.expense, -500, &another);
    let answer = world
        .set_splits(&link.token, &link.transaction, splits)
        .await;

    assert!(
        answer == missing,
        "a hidden goal answers like a missing one"
    );
}

/// The rules still hold a kept link, but a hidden item's own refusal would say
/// what it is: it answers like a missing goal.
#[tokio::test]
async fn kept_hidden_link_breaking_a_rule_answers_like_a_missing_goal() {
    let world = world().await;
    let (_, other) = world.category(CategoryKind::Expense).await;
    for item in [
        with(goal(world.expense), "amount", euros(300_000)),
        one_off(world.expense),
    ] {
        let link = world.hidden_link(item).await;
        let missing = linked(other, -500, PlannedItemId::new());
        let missing = world
            .set_splits(&link.token, &link.transaction, missing)
            .await;

        let splits = linked(other, -500, &link.goal_id);
        let answer = world
            .set_splits(&link.token, &link.transaction, splits)
            .await;

        assert_eq!(answer.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(answer == missing, "a hidden item's refusal says nothing");
    }
}

#[tokio::test]
async fn a_kept_goal_may_move_between_splits() {
    let world = world().await;
    let home = world.account("Home").await;
    let hidden = world.hidden_goal().await;
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, None), (-300, Some(&hidden))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = world.parts(&[(-200, Some(&hidden)), (-300, None)]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["goal"], hidden.as_str());
    assert_eq!(read["splits"][1]["goal"], Value::Null);
}

#[tokio::test]
async fn of_two_linked_goals_the_later_one_may_be_kept_alone() {
    let world = world().await;
    let home = world.account("Home").await;
    let (first, second) = (world.hidden_goal().await, world.hidden_goal().await);
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, Some(&first)), (-300, Some(&second))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = world.parts(&[(-500, Some(&second))]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["goal"], second.as_str());
}

#[tokio::test]
async fn of_two_linked_goals_the_earlier_one_may_be_kept_alone() {
    let world = world().await;
    let home = world.account("Home").await;
    let (first, second) = (world.hidden_goal().await, world.hidden_goal().await);
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, Some(&first)), (-300, Some(&second))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = world.parts(&[(-500, Some(&first))]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["goal"], first.as_str());
}

#[tokio::test]
async fn a_new_hidden_goal_beside_a_kept_one_answers_like_a_missing_goal() {
    let world = world().await;
    let home = world.account("Home").await;
    let (kept, new) = (world.hidden_goal().await, world.hidden_goal().await);
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, None), (-300, Some(&kept))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;
    let absent = PlannedItemId::new().to_string();
    let missing = world.parts(&[(-200, Some(&kept)), (-300, Some(&absent))]);
    let missing = world.set_splits(&token, &transaction, missing).await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let splits = world.parts(&[(-200, Some(&kept)), (-300, Some(&new))]);
    let answer = world.set_splits(&token, &transaction, splits).await;

    assert!(
        answer == missing,
        "a hidden goal answers like a missing one"
    );
}

#[tokio::test]
async fn batch_answers_a_hidden_goal_like_a_missing_one() {
    let world = world().await;
    let home = world.account("Home").await;
    let hidden = world.hidden_goal().await;
    let (first, second) = (
        world.manual(home, -500).await,
        world.manual(home, -700).await,
    );
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;
    let plain = world.parts(&[(-500, None)]);
    let naming = |goal: &str| {
        json!({"operations": [
            {"id": first, "patch": {"splits": plain}},
            {"id": second, "patch": {"splits": linked(world.expense, -700, goal)}},
        ]})
    };
    let uri = format!("{TRANSACTIONS}/batch");
    let absent = naming(&PlannedItemId::new().to_string());
    let missing = world
        .send(Method::POST, &uri, &token, Some(&absent), None)
        .await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let answer = world
        .send(Method::POST, &uri, &token, Some(&naming(&hidden)), None)
        .await;

    assert!(
        answer == missing,
        "a hidden goal answers like a missing one"
    );
}

/// A link kept on one transaction opens its goal to no other in the batch,
/// and the refusal undoes the kept operation too.
#[tokio::test]
async fn batch_keeping_a_hidden_link_refuses_a_hidden_goal_new_to_another_transaction() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let another = world.hidden_goal().await;
    let elsewhere = world.manual(link.home, -700).await;
    let (_, other) = world.category(CategoryKind::Expense).await;
    let naming = |goal: &str| {
        json!({"operations": [
            {"id": link.transaction, "patch": {"splits": linked(other, -500, &link.goal_id)}},
            {"id": elsewhere, "patch": {"splits": linked(world.expense, -700, goal)}},
        ]})
    };
    let uri = format!("{TRANSACTIONS}/batch");
    let absent = naming(&PlannedItemId::new().to_string());
    let missing = world
        .send(Method::POST, &uri, &link.token, Some(&absent), None)
        .await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        missing
            .1
            .contains(&format!("operations[1]: {NO_SUCH_GOAL}"))
    );

    for goal in [another.as_str(), link.goal_id.as_str()] {
        let answer = world
            .send(Method::POST, &uri, &link.token, Some(&naming(goal)), None)
            .await;
        assert!(
            answer == missing,
            "a hidden goal answers like a missing one"
        );
    }
    let untouched = world.read_transaction(&link.transaction).await;
    assert_eq!(untouched["splits"][0]["category"], json!(world.expense));
}

#[tokio::test]
async fn dropping_a_kept_link_unseen_is_audited() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;

    let splits = world.parts(&[(-500, None)]);
    let (status, _) = world
        .set_splits(&link.token, &link.transaction, splits)
        .await;

    assert_eq!(status, StatusCode::OK);
    let (before, after) = world.audited_splits(&link).await;
    let category = world.expense;
    assert_eq!(
        before,
        AuditValue::Text(format!(
            r#"[{{"category":"{category}","minor_units":-500,"goal":"{}"}}]"#,
            link.goal_id
        ))
    );
    assert_eq!(
        after,
        AuditValue::Text(format!(
            r#"[{{"category":"{category}","minor_units":-500}}]"#
        ))
    );
}

#[tokio::test]
async fn changing_a_kept_links_amount_unseen_is_audited() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;

    let splits = world.parts(&[(-200, Some(&link.goal_id)), (-300, None)]);
    let (status, _) = world
        .set_splits(&link.token, &link.transaction, splits)
        .await;

    assert_eq!(status, StatusCode::OK);
    let (before, after) = world.audited_splits(&link).await;
    let (category, goal_id) = (world.expense, &link.goal_id);
    assert_eq!(
        before,
        AuditValue::Text(format!(
            r#"[{{"category":"{category}","minor_units":-500,"goal":"{goal_id}"}}]"#
        ))
    );
    assert_eq!(
        after,
        AuditValue::Text(format!(
            r#"[{{"category":"{category}","minor_units":-200,"goal":"{goal_id}"}},{{"category":"{category}","minor_units":-300}}]"#
        ))
    );
}
