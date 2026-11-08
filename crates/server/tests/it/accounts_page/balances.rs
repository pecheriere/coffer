//! Recording a balance for a day from an account's page, read exactly as a decimal.

use axum::http::{StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::AccountId;

use super::{Reply, World, by_member, input, post, world};

const MARKER: &str = "MarkerZq";

impl World {
    async fn record(&mut self, id: AccountId, on: &str, amount: &str) -> Reply {
        post(
            &mut self.admin,
            &format!("/accounts/{id}/balances"),
            &[("on", on), ("amount", amount)],
        )
        .await
    }

    async fn balances(&mut self, id: AccountId) -> Vec<serde_json::Value> {
        self.api(&format!("/api/v1/accounts/{id}/balances")).await["items"]
            .as_array()
            .unwrap()
            .clone()
    }
}

#[tokio::test]
async fn a_balance_is_recorded_exactly_and_audited() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let reply = world.record(id, "2026-03-04", " 1234.56 ").await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), format!("/accounts/{id}"));
    let balances = world.balances(id).await;
    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0]["on"], "2026-03-04");
    assert_eq!(balances[0]["balance"]["minor_units"], 123_456);
    assert_eq!(balances[0]["source"], "manual");

    let replaced = world.record(id, "2026-03-04", "-0.1").await;
    assert_eq!(replaced.status, StatusCode::SEE_OTHER);
    let balances = world.balances(id).await;
    assert_eq!(balances.len(), 1, "one balance a day");
    assert_eq!(balances[0]["balance"]["minor_units"], -10);

    let audited = world.audited(&id.to_string(), "balance.set").await;
    assert_eq!(audited.len(), 2);
    assert!(audited.iter().all(|event| by_member(event, &world.alice)));
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(page.body.contains("-0.10 USD"));
}

#[tokio::test]
async fn an_amount_is_read_in_the_account_s_currency() {
    let mut world = world().await;
    let id = world
        .holding(input("Yen", AccountKind::Savings, "JPY"))
        .await;
    assert_eq!(
        world.record(id, "2026-03-04", "5000").await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(world.balances(id).await[0]["balance"]["minor_units"], 5000);
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(page.body.contains("Balance in JPY"));

    let refused = world.record(id, "2026-03-05", "1.5").await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains("more decimal places than its currency allows")
    );
}

#[tokio::test]
async fn a_refusal_records_nothing_and_echoes_nothing() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    for (on, amount, words) in [
        ("2026-02-30", "1", "Choose a real day"),
        ("3 March", "1", "Choose a real day"),
        ("2026-03-04", "1,000", "A balance is digits"),
        ("2026-03-04", "12MarkerZq", "A balance is digits"),
        ("2026-03-04", "", "A balance is digits"),
        (
            "2026-03-04",
            "99999999999999999.99",
            "The balance is too large",
        ),
    ] {
        let reply = world.record(id, on, amount).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{words}");
        assert!(reply.body.contains(words), "{words}");
        assert!(!reply.body.contains(MARKER), "never echoes");
    }
    let forged = post(
        &mut world.admin,
        &format!("/accounts/{id}/balances"),
        &[("on", "2026-03-04"), ("amount", "1"), ("source", "sync")],
    )
    .await;
    assert_eq!(forged.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(world.balances(id).await.len(), 0);
    assert_eq!(world.audited(&id.to_string(), "balance.set").await.len(), 0);
}

#[tokio::test]
async fn an_unknown_account_is_not_found() {
    let mut world = world().await;
    let reply = world.record(AccountId::new(), "2026-03-04", "1").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_member_records_as_themselves() {
    let world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let mut bob = world.member().await;
    let reply = post(
        &mut bob,
        &format!("/accounts/{id}/balances"),
        &[("on", "2026-03-04"), ("amount", "10")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let audited = world.audited(&id.to_string(), "balance.set").await;
    assert_eq!(audited.len(), 1);
    assert!(!by_member(&audited[0], &world.alice));
}
