//! What a rule's filing does to plan versus actual: before, after, and after undoing it.

use axum::http::Method;
use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::{World, is_empty, row, usd, world};

const OCTOBER: &str = "2026-10";

impl World {
    async fn rule_run(&self, pattern: &str, category: CategoryId) -> (String, Value) {
        let rule = self
            .admin_send(
                Method::POST,
                "/api/v1/rules",
                &json!({"match_kind": "contains", "pattern": pattern, "category": category}),
            )
            .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let answer = self
            .admin_send(
                Method::POST,
                &format!("/api/v1/rules/{rule}/apply"),
                &json!({}),
            )
            .await;
        (rule, answer)
    }

    async fn description_of(&self, id: &str) -> String {
        let (_, text) = self
            .send(
                Method::GET,
                &format!("/api/v1/transactions/{id}"),
                &self.admin,
                None,
            )
            .await;
        let view: Value = serde_json::from_str(&text).unwrap();
        view["description"].as_str().unwrap().to_owned()
    }

    async fn link_for_rule(&self, out: &str, into: &str) -> Value {
        self.admin_send(
            Method::POST,
            "/api/v1/transfer-links",
            &json!({"outflow": out, "inflow": into}),
        )
        .await
    }

    async fn unfiled(&self, account: AccountId, minor_units: i64) -> String {
        self.manual(account, "2026-10-05", minor_units).await
    }
}

#[tokio::test]
async fn an_expense_rule_moves_money_from_uncategorized_to_actual_and_undoing_moves_it_back() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;
    let before = world.october().await;
    assert_eq!(before["uncategorized"][0]["total"], usd(30_000));

    let (rule, answer) = world.rule_run(&text, food).await;
    assert_eq!(answer["categorized"], 1);

    let after = world.october().await;
    assert_eq!(row(&after, OCTOBER, food)["actual"], usd(30_000));
    assert!(is_empty(&after, "uncategorized"));

    world
        .admin_send(
            Method::POST,
            &format!("/api/v1/rules/{rule}/undo"),
            &json!({}),
        )
        .await;
    assert_eq!(world.october().await, before);
}

#[tokio::test]
async fn a_transfer_rule_takes_money_off_uncategorized_and_adds_no_actual() {
    let world = world().await;
    let checking = world.checking().await;
    let moved = world.category("Moved", "transfer").await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;
    assert_eq!(
        world.october().await["uncategorized"][0]["total"],
        usd(30_000)
    );

    world.rule_run(&text, moved).await;

    let after = world.october().await;
    assert!(is_empty(&after, "uncategorized"));
    let rows = after["rows"].as_array().unwrap();
    assert!(rows.iter().all(|row| row["category"] != json!(moved)));
}

#[tokio::test]
async fn no_rule_changes_a_number_on_one_movement() {
    let world = world().await;
    let checking = world.checking().await;
    let savings = world.holding("Savings", "savings").await;
    let food = world.expense("Food").await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.unfiled(checking, -50_000).await;
    let into = world.unfiled(savings, 50_000).await;
    world.link_for_rule(&out, &into).await;
    let text = world.description_of(&out).await;
    let before = world.october().await;

    let (_, spending) = world.rule_run(&text, food).await;
    assert_eq!(spending["categorized"], 0);
    assert_eq!(world.october().await, before);

    let (_, moving) = world.rule_run(&text, moved).await;
    assert_eq!(moving["categorized"], 2);
    assert_eq!(world.october().await, before);
}

#[tokio::test]
async fn a_transfer_rule_keeps_a_link_out_of_counted_cash_on_uncategorized() {
    let world = world().await;
    let checking = world.checking().await;
    let brokerage = world.holding("Brokerage", "brokerage").await;
    let moved = world.category("Moved", "transfer").await;
    let out = world.unfiled(checking, -50_000).await;
    let into = world.unfiled(brokerage, 50_000).await;
    world.link_for_rule(&out, &into).await;
    let text = world.description_of(&out).await;
    let before = world.october().await;
    assert_eq!(before["uncategorized"][0]["total"], usd(50_000));

    let (_, answer) = world.rule_run(&text, moved).await;

    assert_eq!(answer["categorized"], 0);
    assert_eq!(world.october().await, before);
}
