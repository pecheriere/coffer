//! A goal the token can't see: naming it answers like a missing one, a link to it
//! reads as none and stays on a split of its amount, and every change is audited.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::ids::PlannedItemId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::transactions::CategoryKind;
use serde_json::{Value, json};

use super::super::{World, goal, on, one_off, with, world};
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
async fn read_with_hidden_goal_shows_no_link() {
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
    assert!(!read.contains(&goal_id));
    assert!(!read.contains("Summer trip"));
    let read: Value = serde_json::from_str(&read).unwrap();
    assert_eq!(read["splits"][0]["goal"], Value::Null);
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
async fn an_unseen_link_opens_no_other_hidden_goal() {
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

/// A hidden item's own refusal would say what it is: on the transaction holding
/// the link too, naming it answers like a missing goal.
#[tokio::test]
async fn naming_a_linked_item_breaking_a_rule_answers_like_a_missing_goal() {
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

/// The splits of a read, as the store holds them: each amount and its goal.
fn held(read: &Value) -> Vec<(i64, Option<String>)> {
    read["splits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|split| {
            (
                split["amount"]["minor_units"].as_i64().unwrap(),
                split["goal"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_unseen_link_stays_on_its_amount_when_the_splits_swap() {
    let world = world().await;
    let home = world.account("Home").await;
    let hidden = world.hidden_goal().await;
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, None), (-300, Some(&hidden))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = world.parts(&[(-300, None), (-200, None)]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(held(&read), [(-300, Some(hidden)), (-200, None)]);
}

#[tokio::test]
async fn each_of_two_unseen_links_stays_on_its_own_amount() {
    let world = world().await;
    let home = world.account("Home").await;
    let (first, second) = (world.hidden_goal().await, world.hidden_goal().await);
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, Some(&first)), (-300, Some(&second))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = world.parts(&[(-300, None), (-200, None)]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(held(&read), [(-300, Some(second)), (-200, Some(first))]);
}

/// Of two splits of the amount, the one still in the link's category holds it.
#[tokio::test]
async fn an_unseen_link_prefers_a_split_of_its_own_category() {
    let world = world().await;
    let home = world.account("Home").await;
    let hidden = world.hidden_goal().await;
    let (_, other) = world.category(CategoryKind::Expense).await;
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-250, Some(&hidden)), (-250, None)])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;

    let splits = json!([
        {"category": other, "minor_units": -250},
        {"category": world.expense, "minor_units": -250},
    ]);
    let (status, _) = world.set_splits(&token, &transaction, splits).await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&transaction).await;
    assert_eq!(held(&read), [(-250, None), (-250, Some(hidden))]);
}

/// A link is only ever on an `expense` split, unseen or not, and a refusal would
/// say it is there: on money moved to another kind of category it goes.
#[tokio::test]
async fn an_unseen_link_does_not_follow_money_out_of_expenses() {
    for kind in [CategoryKind::Income, CategoryKind::Transfer] {
        let world = world().await;
        let link = world.hidden_link(goal(world.expense)).await;
        let (_, other) = world.category(kind).await;

        let splits = json!([{"category": other, "minor_units": -500}]);
        let (status, _) = world
            .set_splits(&link.token, &link.transaction, splits)
            .await;

        assert_eq!(status, StatusCode::OK);
        let read = world.read_transaction(&link.transaction).await;
        assert_eq!(held(&read), [(-500, None)]);
    }
}

/// A goal it may see, sent for that money, takes the place of one it can't.
#[tokio::test]
async fn a_visible_goal_sent_replaces_an_unseen_link() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let visible = world
        .create(&with(goal(world.expense), "account_hint", json!(link.home)))
        .await;

    let splits = linked(world.expense, -500, &visible);
    let (status, _) = world
        .set_splits(&link.token, &link.transaction, splits)
        .await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&link.transaction).await;
    assert_eq!(held(&read), [(-500, Some(visible))]);
}

#[tokio::test]
async fn a_new_hidden_goal_beside_an_unseen_link_answers_like_a_missing_goal() {
    let world = world().await;
    let home = world.account("Home").await;
    let (unseen, new) = (world.hidden_goal().await, world.hidden_goal().await);
    let transaction = world.manual(home, -500).await;
    world
        .force_splits(&transaction, &[(-200, None), (-300, Some(&unseen))])
        .await;
    let (_, token) = world.token(Scope::Write, vec![on(home, Write)]).await;
    let absent = PlannedItemId::new().to_string();
    let missing = world.parts(&[(-200, Some(&absent)), (-300, None)]);
    let missing = world.set_splits(&token, &transaction, missing).await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let splits = world.parts(&[(-200, Some(&new)), (-300, None)]);
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
/// and the refusal undoes that operation too.
#[tokio::test]
async fn batch_keeping_an_unseen_link_refuses_a_hidden_goal_on_another_transaction() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;
    let another = world.hidden_goal().await;
    let elsewhere = world.manual(link.home, -700).await;
    let (_, other) = world.category(CategoryKind::Expense).await;
    let naming = |goal: &str| {
        json!({"operations": [
            {"id": link.transaction, "patch": {"splits": [{"category": other, "minor_units": -500}]}},
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
async fn dropping_an_unseen_link_is_audited() {
    let world = world().await;
    let link = world.hidden_link(goal(world.expense)).await;

    let splits = world.parts(&[(-200, None), (-300, None)]);
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
            r#"[{{"category":"{category}","minor_units":-200}},{{"category":"{category}","minor_units":-300}}]"#
        ))
    );
}
