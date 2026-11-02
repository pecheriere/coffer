//! Charges that must not make the number look better: moved money, charges a provider
//! removed or revised, possible duplicates, and spending newer than a balance.

use axum::http::{Method, StatusCode};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::money::DecimalAmount;
use coffer_server::sync::provider::{ProviderAccountData, ProviderTransaction};
use serde_json::{Value, json};

use super::unknowns::{Fed, fed, reported};
use super::{
    BALANCE_ON, TRANSACTIONS, caution, goal_schedule, kinds, line, monthly, outflow, usd, with,
    world,
};

/// The record of a charge, `amount` being the provider's decimal text.
fn charge(id: &str, on: &str, amount: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: id.into(),
        posted_on: Some(on.parse().unwrap()),
        transacted_on: None,
        amount: DecimalAmount::parse(amount).unwrap(),
        amount_raw: amount.into(),
        description: "Charge".into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

impl Fed {
    /// A linked Checking holding 5,000,000 at the end of `BALANCE_ON`, and a floor of 0.
    async fn linked_checking(&self) -> AccountId {
        let checking = self.checking().await;
        self.bound(checking).await;
        self.world.balance(checking, BALANCE_ON, 5_000_000).await;
        checking
    }

    async fn report(&self, records: Vec<ProviderTransaction>, removed: Vec<String>) {
        self.sync(ProviderAccountData {
            transactions: records,
            removed,
            ..reported("USD")
        })
        .await;
    }

    /// The id of the one transaction there is.
    async fn only_transaction(&self) -> String {
        let (_, text) = self
            .world
            .send(Method::GET, TRANSACTIONS, &self.world.admin, None)
            .await;
        let body: Value = serde_json::from_str(&text).unwrap();
        body["items"][0]["id"].as_str().unwrap().to_owned()
    }

    async fn categorize(&self, transaction: &str, splits: &Value) {
        self.world
            .admin_send(
                Method::PATCH,
                &format!("{TRANSACTIONS}/{transaction}"),
                &json!({"splits": splits}),
            )
            .await;
    }

    /// A rent of 200,000 on the 1st, and its November charge, posted and categorized.
    async fn rent_paid(&self) {
        let housing = self.world.expense("Housing").await;
        self.world
            .item(&outflow("Rent", 200_000, housing, &monthly("2026-01-01")))
            .await;
        self.report(vec![charge("rent", "2026-11-01", "-2000.00")], Vec::new())
            .await;
        let id = self.only_transaction().await;
        self.categorize(
            &id,
            &json!([{"category": housing, "minor_units": -200_000}]),
        )
        .await;
    }
}

/// Moving money to savings for a goal isn't spending it: the money is still in cash.
/// A split can't be linked in a `transfer` category, so this one was linked in an
/// `expense` category that was then changed.
#[tokio::test]
async fn a_goal_linked_transfer_is_not_goal_spending() {
    let world = world().await;
    let checking = world.funded("Checking", 2_000_000).await;
    world.funded("Savings", 5_000_000).await;
    world.floor(0).await;
    let moved = world.expense("Moved").await;
    let home = world.expense("Home").await;
    let roof = world
        .item(&outflow(
            "Roof",
            1_000_000,
            home,
            &goal_schedule("2026-01-01", "2027-01-01"),
        ))
        .await;
    world
        .spend_linked(checking, "2026-11-02", 500_000, moved, &roof)
        .await;
    world
        .admin_send(
            Method::PATCH,
            &format!("/api/v1/categories/{moved}"),
            &json!({"kind": "transfer"}),
        )
        .await;

    let forecast = world.forecast().await;

    assert_eq!(
        forecast["surplus"],
        usd(6_000_000),
        "7,000,000 less the roof"
    );
    assert_eq!(line(&forecast, "Roof")["counted"], usd(1_000_000));
}

/// Dec, Jan and Feb are 600,000, and with the charge gone November's 200,000 is unpaid:
/// 5,000,000 - 800,000.
#[tokio::test]
async fn a_removed_charge_no_longer_pays_a_bill() {
    let fed = fed().await;
    fed.linked_checking().await;
    fed.rent_paid().await;

    fed.report(Vec::new(), vec!["rent".into()]).await;

    let forecast = fed.world.forecast().await;
    assert_eq!(forecast["surplus"], usd(4_200_000));
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(caution(&forecast, "bill_not_seen")["amount"], usd(200_000));
}

/// One month ahead holds December's 200,000, and November's is unpaid again:
/// 5,000,000 - 400,000.
#[tokio::test]
async fn a_charge_revised_to_zero_no_longer_pays_a_bill() {
    let fed = fed().await;
    fed.linked_checking().await;
    fed.rent_paid().await;

    fed.report(vec![charge("rent", "2026-11-01", "0.00")], Vec::new())
        .await;

    let forecast = fed.world.forecast_at("?horizon_months=1").await;
    assert_eq!(forecast["surplus"], usd(4_600_000));
    assert_eq!(forecast["status"], "provisional");
    assert_eq!(caution(&forecast, "bill_not_seen")["amount"], usd(200_000));
}

/// A charge split -60,000 / -40,000 is revised to -10,000. How much of that was food is
/// unknown, so none of it reduces the budget: 5,000,000 - 100,000.
#[tokio::test]
async fn a_revised_split_charge_no_longer_reduces_a_budget() {
    let fed = fed().await;
    fed.linked_checking().await;
    let food = fed.world.expense("Food").await;
    let fun = fed.world.expense("Fun").await;
    fed.world
        .item(&with(
            outflow("Groceries", 100_000, food, &monthly("2026-01-28")),
            "spending",
            json!("budget"),
        ))
        .await;
    fed.report(vec![charge("shop", "2026-11-02", "-1000.00")], Vec::new())
        .await;
    let id = fed.only_transaction().await;
    fed.categorize(
        &id,
        &json!([
            {"category": food, "minor_units": -60_000},
            {"category": fun, "minor_units": -40_000},
        ]),
    )
    .await;

    fed.report(vec![charge("shop", "2026-11-02", "-100.00")], Vec::new())
        .await;

    let forecast = fed.world.forecast_at("?horizon_months=1").await;
    assert_eq!(line(&forecast, "Groceries")["reduced_by"], usd(0));
    assert_eq!(forecast["surplus"], usd(4_900_000));
}

/// Removed or revised away from its splits, a charge linked to a goal stops lowering the
/// need: Roof stays 1,000,000.
#[tokio::test]
async fn removed_and_revised_charges_no_longer_lower_a_goal() {
    let fed = fed().await;
    fed.linked_checking().await;
    let (home, fun) = (
        fed.world.expense("Home").await,
        fed.world.expense("Fun").await,
    );
    let roof = fed
        .world
        .item(&outflow(
            "Roof",
            1_000_000,
            home,
            &goal_schedule("2026-01-01", "2027-01-01"),
        ))
        .await;
    fed.report(vec![charge("gone", "2026-11-02", "-3000.00")], Vec::new())
        .await;
    let gone = fed.only_transaction().await;
    fed.categorize(
        &gone,
        &json!([{"category": home, "minor_units": -300_000, "goal": roof}]),
    )
    .await;
    assert_eq!(
        line(&fed.world.forecast().await, "Roof")["counted"],
        usd(700_000),
        "linked, it lowers the need"
    );

    fed.report(Vec::new(), vec!["gone".into()]).await;
    assert_eq!(
        line(&fed.world.forecast().await, "Roof")["counted"],
        usd(1_000_000),
        "removed, it doesn't"
    );

    fed.report(vec![charge("cut", "2026-11-03", "-3000.00")], Vec::new())
        .await;
    let cut = fed.only_transaction().await;
    fed.categorize(
        &cut,
        &json!([
            {"category": home, "minor_units": -200_000, "goal": roof},
            {"category": fun, "minor_units": -100_000},
        ]),
    )
    .await;
    fed.report(vec![charge("cut", "2026-11-03", "-500.00")], Vec::new())
        .await;
    assert_eq!(
        line(&fed.world.forecast().await, "Roof")["counted"],
        usd(1_000_000),
        "revised, it doesn't"
    );
}

/// Two purchases of 300,000 linked to Roof, one of them possibly the other again: only
/// one lowers the need, to 700,000.
#[tokio::test]
async fn a_possible_duplicate_does_not_lower_a_goal() {
    let world = world().await;
    let checking = world.funded("Checking", 5_000_000).await;
    world.floor(0).await;
    let home = world.expense("Home").await;
    let roof = world
        .item(&outflow(
            "Roof",
            1_000_000,
            home,
            &goal_schedule("2026-01-01", "2027-01-01"),
        ))
        .await;
    world
        .spend_linked(checking, "2026-11-02", 300_000, home, &roof)
        .await;
    let flagged = world
        .spend_linked(checking, "2026-11-02", 300_000, home, &roof)
        .await;
    // Planted: a sync's matching has its own tests, and only the flag matters here.
    world
        .server
        .store
        .execute_unchecked_for_tests(
            "UPDATE transactions SET flag_possible_duplicate = 1 WHERE id = ?",
            flagged.as_str().unwrap(),
        )
        .await
        .unwrap();

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(4_300_000));
}

/// A balance from the 10th can't be trusted to show a categorized 1,000,000 on the 14th
/// or an uncategorized 700,000 on the 13th. The number stays, and it says so.
#[tokio::test]
async fn known_spending_newer_than_the_balance_is_a_caution() {
    let world = world().await;
    let checking = world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.balance(checking, "2026-11-10", 5_000_000).await;
    world.floor(0).await;
    let car = world.expense("Car").await;
    world.spend(checking, "2026-11-14", 1_000_000, car).await;
    world.manual(checking, "2026-11-13", -700_000).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(5_000_000), "the number stays");
    assert_eq!(forecast["status"], "provisional", "but it isn't firm");
    assert_eq!(
        kinds(&forecast, "cautions"),
        ["spending_newer_than_balance"]
    );
    let newer = caution(&forecast, "spending_newer_than_balance");
    assert_eq!(newer["currency"], "USD");
    assert_eq!(newer["total"], usd(1_700_000));
}

/// The balance from the 10th shows neither the bill of the 12th nor the 200,000 paid on
/// the 13th, so the bill is still unpaid, on top of December's: 5,000,000 - 400,000.
#[tokio::test]
async fn a_bill_past_its_date_stays_unpaid_with_the_caution() {
    let world = world().await;
    let checking = world
        .account_with("Checking", "checking", "USD", json!({}))
        .await;
    world.balance(checking, "2026-11-10", 5_000_000).await;
    world.floor(0).await;
    let power = world.expense("Power").await;
    world
        .item(&outflow("Power", 200_000, power, &monthly("2026-01-12")))
        .await;
    world.spend(checking, "2026-11-13", 200_000, power).await;

    let forecast = world.forecast_at("?horizon_months=1").await;

    assert_eq!(forecast["surplus"], usd(4_600_000));
    assert_eq!(
        kinds(&forecast, "cautions"),
        ["bill_not_seen", "spending_newer_than_balance"]
    );
}

/// A closed card with no balance isn't in the cash, so a 300,000 purchase on it, linked
/// to Roof, can't raise the surplus: 5,000,000 - 1,000,000.
#[tokio::test]
async fn a_goal_purchase_on_a_closed_card_without_a_balance_changes_nothing() {
    let world = world().await;
    world.funded("Checking", 5_000_000).await;
    world.floor(0).await;
    let home: CategoryId = world.expense("Home").await;
    let roof = world
        .item(&outflow(
            "Roof",
            1_000_000,
            home,
            &goal_schedule("2026-01-01", "2027-01-01"),
        ))
        .await;
    let card = world
        .account_with("Card", "credit_card", "USD", json!({}))
        .await;
    world
        .spend_linked(card, "2026-11-02", 300_000, home, &roof)
        .await;
    world.close(card).await;

    let forecast = world.forecast().await;

    assert_eq!(forecast["surplus"], usd(4_000_000));
}

/// A what-if's item limit is in the document as well as the code.
#[tokio::test]
async fn what_if_documents_its_item_limit() {
    let world = world().await;
    let (status, text) = world
        .send(Method::GET, "/api/v1/openapi.json", &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let document: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        document["components"]["schemas"]["WhatIfRequest"]["properties"]["items"]["maxItems"],
        50
    );
}
