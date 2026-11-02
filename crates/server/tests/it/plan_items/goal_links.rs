//! Splits linked to goals (spec §8.9 R8, R17): what a split may name, that a
//! link survives whatever rewrites its split, what it stops a goal doing, and
//! that a goal the token can't see answers exactly like a missing one.

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, CategoryId, ConnectionId, PlannedItemId};
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_store::{
    AccountRecord, Attribution, AuditEvent, AuditFilter, NewConnection, PageRequest,
    ProviderAccountRecord, ProviderAccountUpsert,
};
use jiff::civil::date;
use serde_json::{Value, json};

use super::{World, goal, item_uri, on, one_off, recurring, with, world};

const TRANSACTIONS: &str = "/api/v1/transactions";
const NO_SUCH_GOAL: &str = "a split names a goal that does not exist";
const HAS_LINKS: &str = "the goal has linked splits; mark it complete instead";

fn transaction_uri(id: &str) -> String {
    format!("{TRANSACTIONS}/{id}")
}

/// One split of `minor_units`, linked to `goal`.
fn linked(category: CategoryId, minor_units: i64, goal: impl serde::Serialize) -> Value {
    json!([{"category": category, "minor_units": minor_units, "goal": goal}])
}

fn euros(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "EUR"})
}

/// A provider account bound to an account of its own.
struct Feed {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    bound: AccountRecord,
}

fn provider_record(id: &str, amount_minor: i64, pending: bool) -> NormalizedTransaction {
    NormalizedTransaction {
        provider_id: id.into(),
        posted_on: date(2026, 10, 10),
        date_reported: true,
        transacted_on: None,
        amount_minor,
        amount_raw: amount_minor.to_string(),
        description: id.into(),
        payee: None,
        memo: None,
        pending,
        replaces_provider_id: None,
    }
}

impl World {
    /// A manual transaction of `minor_units`; returns its id.
    async fn manual(&self, ledger: AccountId, minor_units: i64) -> String {
        let body = json!({
            "account": ledger,
            "posted_on": "2027-01-10",
            "minor_units": minor_units,
            "description": "Purchase",
        });
        let (status, answer) = self
            .call(Method::POST, TRANSACTIONS, &self.admin, Some(&body))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        answer["id"].as_str().unwrap().to_owned()
    }

    /// Sets `splits` on `transaction` as `token`; answers the raw body.
    async fn set_splits(
        &self,
        token: &SecretToken,
        transaction: &str,
        splits: Value,
    ) -> (StatusCode, String) {
        let body = json!({"splits": splits});
        self.send(
            Method::PATCH,
            &transaction_uri(transaction),
            token,
            Some(&body),
            None,
        )
        .await
    }

    /// Links `transaction`'s one split to `goal`, which must succeed.
    async fn link(&self, transaction: &str, minor_units: i64, goal: &str) {
        let splits = linked(self.expense, minor_units, goal);
        let (status, _) = self.set_splits(&self.admin, transaction, splits).await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn read_transaction(&self, id: &str) -> Value {
        let uri = transaction_uri(id);
        let (status, answer) = self.call(Method::GET, &uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    /// The `after` of `field` in `transaction`'s latest audited update.
    async fn audited_after(&self, transaction: &str, field: &str) -> AuditValue {
        self.transaction_audit(transaction)
            .await
            .iter()
            .find(|event| event.action == "transaction.update")
            .and_then(|event| {
                event
                    .changes
                    .iter()
                    .find(|(name, _)| *name == field)
                    .map(|(_, change)| change.after.clone())
            })
            .unwrap()
    }

    /// `transaction`'s audit entries, newest first.
    async fn transaction_audit(&self, transaction: &str) -> Vec<AuditEvent> {
        let filter = AuditFilter {
            entity_type: Some("transaction".into()),
            entity_id: Some(transaction.to_owned()),
            ..AuditFilter::default()
        };
        let page = self
            .server
            .store
            .list_audit(&filter, &PageRequest::default())
            .await;
        let Ok(page) = page else {
            panic!("the audit log could not be read");
        };
        page.items
    }

    async fn delete_item(&self, id: &str) -> (StatusCode, Value) {
        self.call(Method::DELETE, &item_uri(id), &self.admin, None)
            .await
    }

    async fn feed(&self) -> Feed {
        let ledger = self.account("Imported").await;
        let bound = self.server.store.get_account(ledger).await.unwrap();
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        let connection = tx
            .insert_connection(NewConnection {
                provider: "testbank",
                label: ConnectionLabel::parse("Bank").unwrap(),
                owner: self.alice.member,
                credentials: vec![1, 2, 3],
                created_at: now,
            })
            .await
            .unwrap();
        let upserted = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection: connection.id,
                provider_account_id: "acct-1".into(),
                name: "Everyday".into(),
                institution: None,
                currency_raw: "USD".into(),
                kind_hint: Some(AccountKind::Checking),
                balance: None,
                seen_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx.bind_provider_account(upserted.id, ledger).await.unwrap();
        tx.commit().await.unwrap();
        Feed {
            connection: connection.id,
            provider_account,
            bound,
        }
    }

    /// Runs a sync's apply step over `records`.
    async fn apply(&self, feed: &Feed, records: Vec<NormalizedTransaction>) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        apply_account_batch(
            &mut tx,
            now,
            feed.connection,
            AccountBatch {
                provider_account: feed.provider_account.clone(),
                account: Some(feed.bound.clone()),
                transactions: records,
                removed: Vec::new(),
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// The id of the transaction described as `description` in `feed`.
    async fn imported(&self, feed: &Feed, description: &str) -> String {
        let uri = format!("{TRANSACTIONS}?account={}", feed.bound.id);
        let (status, answer) = self.call(Method::GET, &uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::OK);
        answer["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["description"] == description)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

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
async fn imported_amount_change_keeps_goal_link() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(&feed, vec![provider_record("BOOKING", -1_000, false)])
        .await;
    let transaction = world.imported(&feed, "BOOKING").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&transaction, -1_000, &goal_id).await;

    world
        .apply(&feed, vec![provider_record("BOOKING", -1_100, false)])
        .await;

    let read = world.read_transaction(&transaction).await;
    assert_eq!(read["splits"][0]["amount"]["minor_units"], -1_100);
    assert_eq!(read["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn merge_keeps_goal_links() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(
            &feed,
            vec![
                provider_record("AUTHORIZED", -1_000, true),
                provider_record("SETTLED", -1_100, false),
            ],
        )
        .await;
    let target = world.imported(&feed, "AUTHORIZED").await;
    let duplicate = world.imported(&feed, "SETTLED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&target, -1_000, &goal_id).await;

    let uri = format!("{TRANSACTIONS}/{target}/merge");
    let body = json!({"duplicate": duplicate});
    let (status, _) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;

    assert_eq!(status, StatusCode::OK);
    let read = world.read_transaction(&target).await;
    assert_eq!(read["splits"][0]["amount"]["minor_units"], -1_100);
    assert_eq!(read["splits"][0]["goal"], goal_id.as_str());
}

#[tokio::test]
async fn merging_away_a_linked_duplicate_frees_the_goal() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(
            &feed,
            vec![
                provider_record("AUTHORIZED", -1_000, true),
                provider_record("SETTLED", -1_000, false),
            ],
        )
        .await;
    let target = world.imported(&feed, "SETTLED").await;
    let duplicate = world.imported(&feed, "AUTHORIZED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&duplicate, -1_000, &goal_id).await;

    let uri = format!("{TRANSACTIONS}/{target}/merge");
    let body = json!({"duplicate": duplicate, "discard_duplicate_state": true});
    let (status, _) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(world.delete_item(&goal_id).await.0, StatusCode::NO_CONTENT);
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

/// A transaction linked to a goal hinted at a ledger of its own, and a token
/// with `write` on the transaction's ledger that can't see the goal.
struct HiddenLink {
    home: AccountId,
    transaction: String,
    goal_id: String,
    token: SecretToken,
}

impl World {
    async fn hidden_link(&self, item: Value) -> HiddenLink {
        let (home, hidden) = (self.account("Home").await, self.account("Hidden").await);
        let goal_id = self
            .create(&with(item, "account_hint", json!(hidden)))
            .await;
        let transaction = self.manual(home, -500).await;
        self.force_link(&transaction, -500, &goal_id).await;
        let (_, token) = self.token(Scope::Write, vec![on(home, Write)]).await;
        HiddenLink {
            home,
            transaction,
            goal_id,
            token,
        }
    }

    /// Writes a link straight to the store, past the ledger's checks: the
    /// only way to a link the checks would now refuse.
    async fn force_link(&self, transaction: &str, minor_units: i64, goal: &str) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.replace_splits(
            transaction.parse().unwrap(),
            &[SplitInput {
                category: self.expense,
                minor_units,
                goal: Some(goal.parse().unwrap()),
            }],
            Attribution {
                member: Some(self.alice.member),
                agent: None,
                system: None,
                at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
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
async fn unmerge_keeps_goal_links() {
    let world = world().await;
    let feed = world.feed().await;
    world
        .apply(&feed, vec![provider_record("AUTHORIZED", -4_000, true)])
        .await;
    let mut settled = provider_record("SETTLED", -4_600, false);
    settled.replaces_provider_id = Some("AUTHORIZED".into());
    world.apply(&feed, vec![settled]).await;
    let joined = world.imported(&feed, "SETTLED").await;
    let goal_id = world.create(&goal(world.expense)).await;
    world.link(&joined, -4_600, &goal_id).await;
    let read = world.read_transaction(&joined).await;
    let source = read["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["pending"] == false)
        .unwrap()["id"]
        .clone();

    let uri = format!("{TRANSACTIONS}/{joined}/unmerge");
    let body = json!({"source": source});
    let (status, _) = world
        .call(Method::POST, &uri, &world.admin, Some(&body))
        .await;

    assert_eq!(status, StatusCode::CREATED);
    let kept = world.read_transaction(&joined).await;
    assert_eq!(kept["splits"][0]["amount"]["minor_units"], -4_000);
    assert_eq!(kept["splits"][0]["goal"], goal_id.as_str());
}
