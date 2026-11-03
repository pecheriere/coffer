//! What a rule's filing does to the forecast's numbers: before, after, and after undoing it.

use axum::http::Method;
use coffer_domain::ids::{AccountId, CategoryId};
use serde_json::{Value, json};

use super::{World, caution, kinds, line, monthly, outflow, usd, with, world};

const ONE_MONTH: &str = "?horizon_months=1";

impl World {
    /// A rule filing what contains `pattern` under `category`, run by hand.
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

    async fn rule_undone(&self, rule: &str) -> Value {
        self.admin_send(
            Method::POST,
            &format!("/api/v1/rules/{rule}/undo"),
            &json!({}),
        )
        .await
    }

    /// A budget of 400.00 in a new category, and Checking holding 2,000.00.
    async fn budgeted(&self) -> (AccountId, CategoryId) {
        self.floor(0).await;
        let checking = self.funded("Checking", 200_000).await;
        let food = self.expense("Food").await;
        self.item(&with(
            outflow("Groceries", 40_000, food, &monthly("2026-11-20")),
            "spending",
            json!("budget"),
        ))
        .await;
        (checking, food)
    }

    /// A transaction of `minor_units`, uncategorized, its description what the rule reads.
    async fn unfiled(&self, account: AccountId, minor_units: i64) -> String {
        self.manual(account, "2026-11-05", minor_units)
            .await
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn link_for_rule(&self, out: &str, into: &str) -> Value {
        self.admin_send(
            Method::POST,
            "/api/v1/transfer-links",
            &json!({"outflow": out, "inflow": into}),
        )
        .await
    }

    /// Every transaction's description here reads like this.
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
}

#[tokio::test]
async fn an_expense_rule_turns_uncategorized_spending_into_a_lower_budget_and_undoing_turns_it_back()
 {
    let world = world().await;
    let (checking, food) = world.budgeted().await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&before, "uncategorized_spending")["total"],
        usd(30_000)
    );
    assert_eq!(line(&before, "Groceries")["reduced_by"], usd(0));

    let (rule, answer) = world.rule_run(&text, food).await;
    assert_eq!(answer["categorized"], 1);

    let after = world.forecast_at(ONE_MONTH).await;
    assert!(!kinds(&after, "cautions").contains(&"uncategorized_spending".to_owned()));
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(30_000));

    assert_eq!(world.rule_undone(&rule).await["categorized"], 1);
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// The danger this pins: a `transfer` rule clears the caution without lowering a budget, so
/// the money stops being asked about. It is the person's rule to write, and the docs say so.
#[tokio::test]
async fn a_transfer_rule_clears_the_caution_and_lowers_no_budget() {
    let world = world().await;
    let (checking, _food) = world.budgeted().await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;
    let moved = world.category("Moved", "transfer").await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&before, "uncategorized_spending")["total"],
        usd(30_000)
    );

    let (_, answer) = world.rule_run(&text, moved).await;
    assert_eq!(answer["categorized"], 1);

    let after = world.forecast_at(ONE_MONTH).await;
    assert!(!kinds(&after, "cautions").contains(&"uncategorized_spending".to_owned()));
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(0));
}

#[tokio::test]
async fn an_income_rule_leaves_the_caution_up() {
    let world = world().await;
    let (checking, _food) = world.budgeted().await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;
    let pay = world.category("Pay", "income").await;
    let before = world.forecast_at(ONE_MONTH).await;

    let (_, answer) = world.rule_run(&text, pay).await;

    assert_eq!(answer["left_alone"]["income_on_outflow"], 1);
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// A rule's filing is as good as a person's: the charge pays the bill it fits, all of it leaves the
/// budget's pool, and undoing the rule puts the bill back to owing.
#[tokio::test]
async fn a_rule_filing_a_payment_pays_the_bill_and_undoing_it_owes_again() {
    let world = world().await;
    let (checking, food) = world.budgeted().await;
    world
        .item(&outflow("Box", 30_000, food, &monthly("2026-11-03")))
        .await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&before, "Box")["unpaid"], usd(30_000));

    let (rule, _) = world.rule_run(&text, food).await;

    let after = world.forecast_at(ONE_MONTH).await;
    let bill = line(&after, "Box");
    assert_eq!(bill["unpaid"], usd(0));
    assert_eq!(bill["payments"][0]["paid_by"]["transaction"], json!(id));
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(0));

    world.rule_undone(&rule).await;
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// Money coming in that a rule files under an `expense` category is a refund, so one the size of a
/// charge undoes it and the bill owes again: the safe direction, if the rule was too broad.
#[tokio::test]
async fn a_rule_filing_money_in_as_a_refund_can_unpay_a_bill() {
    let world = world().await;
    let (checking, food) = world.budgeted().await;
    world
        .item(&outflow("Box", 30_000, food, &monthly("2026-11-03")))
        .await;
    world
        .spend_linked(checking, "2026-11-05", 30_000, food, &Value::Null)
        .await;
    let id = world.unfiled(checking, 30_000).await;
    let text = world.description_of(&id).await;

    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&before, "Box")["unpaid"], usd(0));

    let (rule, answer) = world.rule_run(&text, food).await;
    assert_eq!(answer["categorized"], 1);
    assert_eq!(
        line(&world.forecast_at(ONE_MONTH).await, "Box")["unpaid"],
        usd(30_000)
    );

    world.rule_undone(&rule).await;
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// An `income` rule is refused on money going out, so that money pays no bill either.
#[tokio::test]
async fn an_income_rule_refused_on_an_outflow_pays_no_bill() {
    let world = world().await;
    let (checking, food) = world.budgeted().await;
    world
        .item(&outflow("Box", 30_000, food, &monthly("2026-11-03")))
        .await;
    let id = world.unfiled(checking, -30_000).await;
    let text = world.description_of(&id).await;
    let pay = world.category("Pay", "income").await;

    let (_, answer) = world.rule_run(&text, pay).await;

    assert_eq!(answer["left_alone"]["income_on_outflow"], 1);
    let after = world.forecast_at(ONE_MONTH).await;
    assert_eq!(line(&after, "Box")["unpaid"], usd(30_000));
    assert!(kinds(&after, "cautions").contains(&"uncategorized_spending".to_owned()));
}

#[tokio::test]
async fn no_rule_changes_a_number_on_one_movement() {
    let world = world().await;
    let (checking, food) = world.budgeted().await;
    let savings = world.funded("Savings", 100_000).await;
    let out = world.unfiled(checking, -30_000).await;
    let into = world.unfiled(savings, 30_000).await;
    world.link_for_rule(&out, &into).await;
    let text = world.description_of(&out).await;
    let before = world.forecast_at(ONE_MONTH).await;

    let (_, spending) = world.rule_run(&text, food).await;
    assert_eq!(spending["categorized"], 0);
    assert_eq!(spending["left_alone"]["transfer_leg"], 2);
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);

    let moved = world.category("Moved", "transfer").await;
    let (_, moving) = world.rule_run(&text, moved).await;
    assert_eq!(moving["categorized"], 2);
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}

/// Money sent to a brokerage is real spending (§8.11 B9): a `transfer` rule would take it off
/// the caution, so it leaves the field for a person.
#[tokio::test]
async fn a_transfer_rule_keeps_a_link_out_of_counted_cash_on_the_caution() {
    let world = world().await;
    let (checking, _food) = world.budgeted().await;
    let brokerage = world
        .account_with("Brokerage", "brokerage", "USD", json!({}))
        .await;
    let out = world.unfiled(checking, -50_000).await;
    let into = world.unfiled(brokerage, 50_000).await;
    world.link_for_rule(&out, &into).await;
    let text = world.description_of(&out).await;
    let moved = world.category("Moved", "transfer").await;
    let before = world.forecast_at(ONE_MONTH).await;
    assert_eq!(
        caution(&before, "uncategorized_spending")["total"],
        usd(50_000)
    );

    let (_, answer) = world.rule_run(&text, moved).await;

    assert_eq!(answer["categorized"], 0);
    assert_eq!(answer["left_alone"]["transfer_leg"], 2);
    assert_eq!(world.forecast_at(ONE_MONTH).await, before);
}
