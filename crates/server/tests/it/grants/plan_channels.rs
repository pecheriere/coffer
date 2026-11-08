//! Channels the plan and forecast could open past the obvious routes: a hidden
//! goal's id on a visible split, every answer that shows a split, refusals that
//! depend on what exists, and tokens whose grants mix levels or change.

use super::{World, grant, on, s, world};
use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId, TransactionId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{PlanDirection, PlanSchedule, PlanTier, PlannedItemName};
use coffer_domain::policy::{AccountLevel, GrantTarget};
use coffer_domain::time::Clock as _;
use coffer_server::secrets::SecretToken;
use coffer_store::NewPlannedItem;
use jiff::civil::date;
use serde_json::{Value, json};

const PLAN_ITEMS: &str = "/api/v1/plan-items";
const HIDDEN_GOAL_NAME: &str = "Quiet fund";
const HIDDEN_GOAL_MINOR: i64 = 7_346_195_280;

/// A token with `write` on one account, a transaction of -1000 on it linked to a
/// goal hinted at an account the token isn't granted, and the goal's id.
struct LinkedToHidden {
    world: World,
    visible: AccountId,
    category: CategoryId,
    transaction: TransactionId,
    goal: PlannedItemId,
    token: SecretToken,
}

async fn linked_to_hidden() -> LinkedToHidden {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let category = world.expense_category().await;
    let goal = world
        .planned_item(HIDDEN_GOAL_NAME, Some(hidden), category, HIDDEN_GOAL_MINOR)
        .await;
    let transaction = world.transaction(visible, 1, "Flights").await;
    world.link_split(transaction, category, -1000, goal).await;
    let (_, token) = world
        .token(Scope::Write, vec![on(visible, AccountLevel::Write)])
        .await;
    LinkedToHidden {
        world,
        visible,
        category,
        transaction,
        goal,
        token,
    }
}

/// `body` as it reads with every split's link taken off: what a token that can't
/// see the goal must get, whole.
fn without_links(mut body: Value) -> Value {
    for split in body["splits"].as_array_mut().unwrap() {
        split["goal"] = Value::Null;
    }
    body
}

impl LinkedToHidden {
    fn uri(&self) -> String {
        format!("/api/v1/transactions/{}", self.transaction)
    }

    /// The transaction as alice's session sees it, link included.
    async fn as_session(&self) -> Value {
        let view = self
            .world
            .server
            .ledger
            .get_transaction(&self.world.alice, self.transaction)
            .await
            .unwrap();
        serde_json::to_value(view).unwrap()
    }
}

#[tokio::test]
async fn a_read_of_a_split_says_nothing_of_a_goal_it_cannot_see() {
    let linked = linked_to_hidden().await;
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["goal"], s(linked.goal));

    let (status, read) = linked.world.get(&linked.uri(), &linked.token).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, without_links(session));
}

#[tokio::test]
async fn no_list_or_write_answer_names_a_goal_it_cannot_see() {
    let linked = linked_to_hidden().await;
    let LinkedToHidden { world, token, .. } = &linked;
    let hidden_id = s(linked.goal);
    let list = format!("/api/v1/transactions?account={}", linked.visible);
    let batch = json!({"operations": [{"id": linked.transaction, "patch": {"notes": "batched"}}]});
    let answers = [
        world.send(Method::GET, &list, token, None, None).await,
        world
            .send(
                Method::PATCH,
                &linked.uri(),
                token,
                Some(&json!({"notes": "looked"})),
                None,
            )
            .await,
        world
            .send(
                Method::POST,
                "/api/v1/transactions/batch",
                token,
                Some(&batch),
                None,
            )
            .await,
        world
            .send(
                Method::POST,
                &format!("{}/dismiss-duplicate", linked.uri()),
                token,
                None,
                None,
            )
            .await,
    ];
    for (status, text) in answers {
        assert_eq!(status, StatusCode::OK);
        assert!(!text.contains(&hidden_id), "a hidden goal's id stays out");
    }
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["goal"], hidden_id, "the link stays");
}

/// A transaction of -2500 with two provider records, flagged as a possible
/// duplicate or merged by `token`, linked to a goal `token` can't see.
struct ImportedLink {
    world: World,
    goal: PlannedItemId,
    target: TransactionId,
    duplicate: TransactionId,
    duplicate_source: String,
    token: SecretToken,
}

async fn imported_link() -> ImportedLink {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let category = world.expense_category().await;
    let goal = world
        .planned_item(HIDDEN_GOAL_NAME, Some(hidden), category, 5000)
        .await;
    let target_feed = world.connection(&[("Target", Some(visible))]).await;
    let duplicate_feed = world.connection(&[("Duplicate", Some(visible))]).await;
    let (target, _) = world.imported(target_feed, visible, "Groceries").await;
    let (duplicate, duplicate_source) = world.imported(duplicate_feed, visible, "Groceries").await;
    world.link_split(target, category, -2500, goal).await;
    world.flag_duplicate(target, duplicate).await;
    let (_, token) = world
        .token(Scope::Write, vec![on(visible, AccountLevel::Write)])
        .await;
    ImportedLink {
        world,
        goal,
        target,
        duplicate,
        duplicate_source,
        token,
    }
}

impl ImportedLink {
    async fn merge(&self) -> (StatusCode, String) {
        self.world
            .send(
                Method::POST,
                &format!("/api/v1/transactions/{}/merge", self.target),
                &self.token,
                Some(&json!({"duplicate": self.duplicate})),
                None,
            )
            .await
    }

    async fn held_goal(&self) -> Option<PlannedItemId> {
        let held = self
            .world
            .server
            .ledger
            .get_transaction(&self.world.alice, self.target)
            .await
            .unwrap();
        held.splits[0].goal
    }
}

/// A merge answers with its target, so with the target's links.
#[tokio::test]
async fn a_merge_answer_names_no_goal_it_cannot_see() {
    let imported = imported_link().await;

    let (status, text) = imported.merge().await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        !text.contains(&s(imported.goal)),
        "a hidden goal's id stays out"
    );
    assert_eq!(imported.held_goal().await, Some(imported.goal));
}

/// An unmerge answers with what it split off, which has no splits of its own.
#[tokio::test]
async fn an_unmerge_answer_names_no_goal_it_cannot_see() {
    let imported = imported_link().await;
    let (status, _) = imported.merge().await;
    assert_eq!(status, StatusCode::OK);

    let (status, text) = imported
        .world
        .send(
            Method::POST,
            &format!("/api/v1/transactions/{}/unmerge", imported.target),
            &imported.token,
            Some(&json!({"source": imported.duplicate_source})),
            None,
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(
        !text.contains(&s(imported.goal)),
        "a hidden goal's id stays out"
    );
    assert_eq!(imported.held_goal().await, Some(imported.goal));
}

/// Dismissing a flagged transaction writes, and answers from a path of its own.
#[tokio::test]
async fn dismissing_a_flag_names_no_goal_it_cannot_see() {
    let imported = imported_link().await;

    let (status, text) = imported
        .world
        .send(
            Method::POST,
            &format!("/api/v1/transactions/{}/dismiss-duplicate", imported.target),
            &imported.token,
            None,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        !text.contains(&s(imported.goal)),
        "a hidden goal's id stays out"
    );
    assert_eq!(imported.held_goal().await, Some(imported.goal));
}

/// Sending back what it read writes nothing, exactly as on a split with no link,
/// so a no-op can't tell the two apart.
#[tokio::test]
async fn resending_what_it_read_changes_nothing() {
    let linked = linked_to_hidden().await;
    let LinkedToHidden { world, token, .. } = &linked;
    let (_, read) = world.get(&linked.uri(), token).await;
    let audited = world.audit_count().await;

    let splits = json!({"splits": read["splits"].as_array().unwrap().iter().map(|split| json!({
        "category": split["category"],
        "minor_units": split["amount"]["minor_units"],
    })).collect::<Vec<_>>()});
    let (status, answer) = world
        .call(Method::PATCH, &linked.uri(), token, Some(splits))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer, read, "the same transaction, whole");
    assert_eq!(world.audit_count().await, audited, "nothing was written");
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["goal"], s(linked.goal));
}

/// A goal-less split keeps a link it can't see: a token categorizing the money
/// doesn't move a hidden goal's figures by not knowing it.
#[tokio::test]
async fn recategorizing_a_split_keeps_a_link_it_cannot_see() {
    let linked = linked_to_hidden().await;
    let other = linked.world.expense_category().await;
    assert_ne!(other, linked.category);

    let (status, answer) = linked
        .world
        .call(
            Method::PATCH,
            &linked.uri(),
            &linked.token,
            Some(json!({"splits": [{"category": other, "minor_units": -1000}]})),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["splits"][0]["category"], s(other));
    assert_eq!(answer["splits"][0]["goal"], Value::Null);
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["category"], s(other));
    assert_eq!(session["splits"][0]["goal"], s(linked.goal));
}

/// Without a split of that amount there is nothing to keep it on: it goes, and the
/// audit log, which only a token seeing every account reads, says so.
#[tokio::test]
async fn splitting_the_money_differently_drops_the_link_and_is_audited() {
    let linked = linked_to_hidden().await;
    let category = linked.category;

    let (status, _) = linked
        .world
        .call(
            Method::PATCH,
            &linked.uri(),
            &linked.token,
            Some(json!({"splits": [
                {"category": category, "minor_units": -400},
                {"category": category, "minor_units": -600},
            ]})),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["goal"], Value::Null);
    assert_eq!(session["splits"][1]["goal"], Value::Null);
    let (_, wide) = linked
        .world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let (_, audit) = linked
        .world
        .get(
            &format!(
                "/api/v1/audit?entity_type=transaction&entity_id={}",
                linked.transaction
            ),
            &wide,
        )
        .await;
    assert_eq!(audit["items"][0]["action"], "transaction.update");
    let before = audit["items"][0]["changes"]["splits"]["before"]
        .as_str()
        .unwrap();
    assert!(before.contains(&s(linked.goal)), "the link it had");
}

/// A token can't confirm a link by naming the goal, even on the very transaction
/// that holds it.
#[tokio::test]
async fn naming_the_hidden_goal_of_its_own_transaction_answers_as_missing() {
    let linked = linked_to_hidden().await;
    let naming = |goal: PlannedItemId| json!({"splits": [{"category": linked.category, "minor_units": -1000, "goal": goal}]});
    let uri = linked.uri();
    let missing = linked
        .world
        .send(
            Method::PATCH,
            &uri,
            &linked.token,
            Some(&naming(PlannedItemId::new())),
            None,
        )
        .await;
    assert_eq!(missing.0, StatusCode::UNPROCESSABLE_ENTITY);

    let answer = linked
        .world
        .send(
            Method::PATCH,
            &uri,
            &linked.token,
            Some(&naming(linked.goal)),
            None,
        )
        .await;

    assert!(answer == missing, "answered like a missing goal");
}

#[tokio::test]
async fn a_narrowed_grant_hides_a_link_at_the_next_read() {
    let world = world().await;
    let (visible, hinted) = (
        world.account("Checking").await,
        world.account("Hinted").await,
    );
    let category = world.expense_category().await;
    let goal = world
        .planned_item(HIDDEN_GOAL_NAME, Some(hinted), category, 5000)
        .await;
    let transaction = world.transaction(visible, 1, "Flights").await;
    world.link_split(transaction, category, -1000, goal).await;
    let (id, token) = world
        .token(
            Scope::Read,
            vec![
                on(visible, AccountLevel::Transactions),
                on(hinted, AccountLevel::Transactions),
            ],
        )
        .await;
    let uri = format!("/api/v1/transactions/{transaction}");
    let (_, read) = world.get(&uri, &token).await;
    assert_eq!(read["splits"][0]["goal"], s(goal), "while it may see it");

    world
        .server
        .agent_tokens
        .replace_grants(
            &world.alice,
            id,
            vec![
                on(visible, AccountLevel::Transactions),
                on(hinted, AccountLevel::Balances),
            ],
        )
        .await
        .unwrap();

    let (_, read) = world.get(&uri, &token).await;
    assert_eq!(read["splits"][0]["goal"], Value::Null);
}

/// What a member's session sees of a link and what its token sees: only the
/// token's grants differ.
#[tokio::test]
async fn a_session_sees_what_its_restricted_token_does_not() {
    let linked = linked_to_hidden().await;
    let LinkedToHidden { world, goal, .. } = &linked;

    let item = world
        .server
        .ledger
        .get_planned_item(&world.alice, *goal)
        .await
        .unwrap();
    assert_eq!(item.name, HIDDEN_GOAL_NAME);
    let session = linked.as_session().await;
    assert_eq!(session["splits"][0]["goal"], s(*goal));

    let (status, _) = world
        .get(&format!("{PLAN_ITEMS}/{goal}"), &linked.token)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, read) = world.get(&linked.uri(), &linked.token).await;
    assert_eq!(read["splits"][0]["goal"], Value::Null);
}

/// Status, every header and the body: a hidden item and a missing one differ in
/// nothing a client receives.
#[tokio::test]
async fn a_hidden_item_and_a_missing_one_answer_alike_to_the_header() {
    let linked = linked_to_hidden().await;
    let hidden = linked
        .world
        .get_whole(&format!("{PLAN_ITEMS}/{}", linked.goal), &linked.token)
        .await;
    let missing = linked
        .world
        .get_whole(
            &format!("{PLAN_ITEMS}/{}", PlannedItemId::new()),
            &linked.token,
        )
        .await;

    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert!(hidden == missing, "the whole answer is the same");
}

/// The kind refusal covers goal-linked splits, so it must be out of reach of a
/// token that can't see every split: the same 403 with or without the link.
#[tokio::test]
async fn a_kind_refusal_cannot_reveal_a_hidden_goal_link() {
    let linked = linked_to_hidden().await;
    let LinkedToHidden { world, .. } = &linked;
    let unused = world.expense_category().await;
    let uri = |category: CategoryId| format!("/api/v1/categories/{category}");
    let patch = json!({"kind": "income"});
    for grants in [
        vec![on(linked.visible, AccountLevel::Write)],
        vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        vec![
            grant(GrantTarget::All, AccountLevel::Balances),
            on(linked.visible, AccountLevel::Write),
        ],
    ] {
        let (_, token) = world.token(Scope::Write, grants).await;
        let used_answer = world
            .send(
                Method::PATCH,
                &uri(linked.category),
                &token,
                Some(&patch),
                None,
            )
            .await;
        let unused_answer = world
            .send(Method::PATCH, &uri(unused), &token, Some(&patch), None)
            .await;
        assert_eq!(used_answer.0, StatusCode::FORBIDDEN);
        assert!(used_answer == unused_answer, "the same refusal either way");
    }

    // What the refusal would have said to a token that may see all of it.
    let (_, wide) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let (status, _) = world
        .call(
            Method::PATCH,
            &uri(linked.category),
            &wide,
            Some(patch.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = world
        .call(Method::PATCH, &uri(unused), &wide, Some(patch))
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// The household's 500-item limit counts hidden items, so it must not be reachable
/// by a token that can't see them.
#[tokio::test]
async fn the_item_limit_cannot_be_used_to_count_hidden_items() {
    let world = world().await;
    let (visible, hidden) = (
        world.account("Checking").await,
        world.account("Hidden").await,
    );
    let category = world.expense_category().await;
    let (_, restricted) = world
        .token(Scope::Write, vec![on(visible, AccountLevel::Write)])
        .await;
    let (_, wide) = world
        .token(
            Scope::Write,
            vec![grant(GrantTarget::All, AccountLevel::Write)],
        )
        .await;
    let body = json!({
        "name": "One more",
        "category": category,
        "amount": {"minor_units": 100, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "one_off", "on": "2027-03-15"},
    });
    let below = world
        .send(Method::POST, PLAN_ITEMS, &restricted, Some(&body), None)
        .await;
    assert_eq!(below.0, StatusCode::FORBIDDEN);

    fill_the_plan(&world, category, hidden).await;

    let at_the_limit = world
        .send(Method::POST, PLAN_ITEMS, &restricted, Some(&body), None)
        .await;
    assert!(at_the_limit == below, "the same refusal either way");
    let (status, _) = world
        .call(Method::POST, PLAN_ITEMS, &wide, Some(body))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "the limit does hold");
}

/// 500 items hinted at `hidden`, all in one store transaction.
async fn fill_the_plan(world: &World, category: CategoryId, hidden: AccountId) {
    let now = world.server.clock.now();
    let mut tx = world.server.store.begin().await.unwrap();
    for _ in 0..500 {
        tx.insert_planned_item(NewPlannedItem {
            name: PlannedItemName::parse("Filler").unwrap(),
            category,
            amount: Money {
                minor_units: 100,
                currency: Currency::parse("USD").unwrap(),
            },
            direction: PlanDirection::Outflow,
            account_hint: Some(hidden),
            notes: None,
            active: true,
            schedule: PlanSchedule::OneOff {
                on: date(2027, 3, 15),
            },
            spending: None,
            tier: Some(PlanTier::MustPay),
            created_at: now,
        })
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

/// An `all` grant at one level beside named grants at another: the item decides by
/// its own account's best level, whatever else the token holds.
#[tokio::test]
async fn all_at_one_level_and_named_at_another() {
    let world = world().await;
    let (named, rest) = (world.account("Named").await, world.account("Rest").await);
    let category = world.expense_category().await;
    let on_named = world
        .planned_item("On named", Some(named), category, 5000)
        .await;
    let on_rest = world
        .planned_item("On rest", Some(rest), category, 5000)
        .await;
    let unhinted = world.planned_item("Unhinted", None, category, 5000).await;
    let spans = |grants| world.token(Scope::Write, grants);
    let seen = async |token: &SecretToken| {
        let (status, listed) = world.get(PLAN_ITEMS, token).await;
        assert_eq!(status, StatusCode::OK);
        super::ids(&listed)
    };
    let all = |level| grant(GrantTarget::All, level);

    // `all` below `transactions`, one named above it: that one, and unhinted items.
    let (_, token) = spans(vec![
        all(AccountLevel::Balances),
        on(named, AccountLevel::Transactions),
    ])
    .await;
    assert_eq!(seen(&token).await, [s(on_named), s(unhinted)]);
    let (status, _) = world.get("/api/v1/forecast", &token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // `all` at `transactions`, named lower: the better level wins on every account.
    let (_, token) = spans(vec![
        all(AccountLevel::Transactions),
        on(named, AccountLevel::Balances),
    ])
    .await;
    assert_eq!(seen(&token).await, [s(on_named), s(on_rest), s(unhinted)]);
    let (status, _) = world.get("/api/v1/forecast", &token).await;
    assert_eq!(status, StatusCode::OK);

    // Named at `write` on every account that exists is still not `all`.
    let (_, token) = spans(vec![
        on(named, AccountLevel::Write),
        on(rest, AccountLevel::Write),
    ])
    .await;
    for uri in ["/api/v1/forecast", "/api/v1/audit"] {
        let (status, _) = world.get(uri, &token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = world
        .call(
            Method::DELETE,
            &format!("{PLAN_ITEMS}/{unhinted}"),
            &token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A read token never writes, whatever `all` grant its row holds: it may ask a
/// what-if, which persists nothing, and may not touch the plan.
#[tokio::test]
async fn a_read_token_with_a_write_grant_on_record_only_reads() {
    let world = world().await;
    let a = world.account("Checking").await;
    let category = world.expense_category().await;
    let item = world.planned_item("Stays", Some(a), category, 5000).await;
    let (id, token) = world
        .token(
            Scope::Read,
            vec![grant(GrantTarget::All, AccountLevel::Transactions)],
        )
        .await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.replace_token_grants(id, &[grant(GrantTarget::All, AccountLevel::Write)])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let audited = world.audit_count().await;

    let (status, answer) = world
        .call(
            Method::POST,
            "/api/v1/forecast/what-if",
            &token,
            Some(json!({"items": [{
                "name": "Imagined roof",
                "amount": {"minor_units": 100, "currency": "USD"},
                "direction": "outflow",
                "account_hint": a,
                "schedule": {"type": "one_off", "on": "2026-12-20"},
            }]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(answer.is_object());

    let (status, _) = world
        .call(
            Method::DELETE,
            &format!("{PLAN_ITEMS}/{item}"),
            &token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(world.audit_count().await, audited, "nothing was written");
    let (_, listed) = world.get(PLAN_ITEMS, &token).await;
    assert_eq!(super::ids(&listed), [s(item)], "nothing was added");
}

/// A cursor issued while a token saw more continues only over what it sees now.
#[tokio::test]
async fn a_cursor_from_before_a_narrowed_grant_shows_only_what_it_sees_now() {
    let world = world().await;
    let (a, b) = (
        world.account("Checking").await,
        world.account("Narrowed away").await,
    );
    let older = world.transaction(a, 1, "Older here").await;
    let hidden = world.transaction(b, 2, "Middle there").await;
    let newest = world.transaction(a, 3, "Newest here").await;
    let (id, token) = world
        .token(
            Scope::Read,
            vec![
                on(a, AccountLevel::Transactions),
                on(b, AccountLevel::Transactions),
            ],
        )
        .await;
    let (_, page) = world.get("/api/v1/transactions?limit=1", &token).await;
    assert_eq!(page["items"][0]["id"], s(newest));
    let next = page["next"].as_str().unwrap().to_owned();

    world
        .server
        .agent_tokens
        .replace_grants(&world.alice, id, vec![on(a, AccountLevel::Transactions)])
        .await
        .unwrap();

    let (status, text) = world
        .send(
            Method::GET,
            &format!("/api/v1/transactions?limit=10&after={next}"),
            &token,
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(super::ids(&page), [s(older)]);
    assert!(
        !text.contains(&s(hidden)),
        "the narrowed-away row stays out"
    );
}

/// The refusal of the forecast's routes and of the report says nothing of what
/// exists: a household with hidden money answers a restricted token, byte for
/// byte, as an empty one does.
#[tokio::test]
async fn the_refusals_are_the_same_in_an_empty_household() {
    let populated = linked_to_hidden().await;
    let empty = world().await;
    let empty_visible = empty.account("Checking").await;
    let (_, empty_token) = empty
        .token(Scope::Write, vec![on(empty_visible, AccountLevel::Write)])
        .await;
    let roof = json!({"items": [{
        "name": "Roof",
        "amount": {"minor_units": 100, "currency": "USD"},
        "direction": "outflow",
        "schedule": {"type": "one_off", "on": "2026-12-20"},
    }]});
    for (method, uri, body) in [
        (Method::GET, "/api/v1/forecast", None),
        (Method::POST, "/api/v1/forecast/what-if", Some(&roof)),
        (
            Method::GET,
            "/api/v1/reports/plan-vs-actual?from=2026-10",
            None,
        ),
    ] {
        let full = populated
            .world
            .send(method.clone(), uri, &populated.token, body, None)
            .await;
        let none = empty.send(method, uri, &empty_token, body, None).await;
        assert_eq!(full.0, StatusCode::FORBIDDEN);
        assert!(full == none, "the same refusal whatever exists");
    }
}
