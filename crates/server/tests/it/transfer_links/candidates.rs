//! Finding legs: suggestions a person or agent confirms, and the legs filed as `transfer` with
//! nothing on the other side. Nothing here links anything.

use axum::http::{Method, StatusCode};
use coffer_domain::policy::AccountLevel::Transactions;
use serde_json::{Value, json};

use super::{TRANSACTIONS, World, on, world};

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

impl World {
    async fn candidates(&self, transaction: &str) -> Vec<String> {
        let (status, page) = self
            .send(
                Method::GET,
                &format!("{TRANSACTIONS}/{transaction}/transfer-candidates"),
                &self.admin,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        ids(&page)
    }
}

/// The same amount the other way, on another account, within five days, nearest first; and
/// asking links nothing.
#[tokio::test]
async fn candidates_are_the_opposite_amount_nearby() {
    let world = world().await;
    let leg = world.manual(world.checking, "2026-10-10", -50_000).await;
    let euros = world.account("Euros", "savings", "EUR").await;

    let far = world.manual(world.savings, "2026-10-15", 50_000).await;
    let near = world.manual(world.savings, "2026-10-09", 50_000).await;
    // None of these.
    world.manual(world.savings, "2026-10-16", 50_000).await;
    world.manual(world.savings, "2026-10-04", 50_000).await;
    world.manual(world.savings, "2026-10-10", 49_999).await;
    world.manual(world.savings, "2026-10-10", -50_000).await;
    world.manual(world.checking, "2026-10-10", 50_000).await;
    world.manual(euros, "2026-10-10", 50_000).await;

    assert_eq!(world.candidates(&leg).await, [near.clone(), far]);
    assert_eq!(
        world.transaction(&leg).await["transfer_link"],
        Value::Null,
        "a suggestion applies nothing"
    );
    assert_eq!(world.transaction(&near).await["transfer_link"], Value::Null);
}

/// What couldn't be linked isn't suggested: a transaction in a link, or split in two.
#[tokio::test]
async fn candidates_leave_out_what_cant_be_linked() {
    let world = world().await;
    let leg = world.manual(world.checking, "2026-10-10", -50_000).await;
    let linked = world.manual(world.savings, "2026-10-10", 50_000).await;
    let other_out = world.manual(world.checking, "2026-10-10", -50_000).await;
    world.link(&other_out, &linked).await;
    let split = world.manual(world.savings, "2026-10-10", 50_000).await;
    let pay = world.category("Pay", "income").await;
    world
        .splits(
            &split,
            &json!([
                {"category": pay, "minor_units": 20_000},
                {"category": pay, "minor_units": 30_000},
            ]),
        )
        .await;

    assert_eq!(world.candidates(&leg).await, [""; 0]);
    assert_eq!(
        world.candidates(&other_out).await,
        [""; 0],
        "already linked itself"
    );
}

/// A candidate on an account the token can't read isn't suggested, and a transaction it can't
/// read answers as missing.
#[tokio::test]
async fn candidates_follow_the_tokens_grants() {
    let world = world().await;
    let leg = world.manual(world.checking, "2026-10-10", -50_000).await;
    let hidden = world.manual(world.savings, "2026-10-10", 50_000).await;
    let token = world.token(vec![on(world.checking, Transactions)]).await;

    let uri = format!("{TRANSACTIONS}/{leg}/transfer-candidates");
    let (status, page) = world.send(Method::GET, &uri, &token, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&page), [""; 0]);

    let uri = format!("{TRANSACTIONS}/{hidden}/transfer-candidates");
    let (status, _) = world.send(Method::GET, &uri, &token, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Filed as a `transfer` with no link is what an agent has left to match; a link, or another
/// kind of category, takes it off the list.
#[tokio::test]
async fn unmatched_legs_are_filed_as_transfer_with_no_link() {
    let world = world().await;
    let moved = world.category("Moved", "transfer").await;
    let food = world.category("Food", "expense").await;
    let (outflow, inflow) = world.pair().await;
    let unfiled = world.manual(world.checking, "2026-10-06", -1_000).await;
    let purchase = world.manual(world.checking, "2026-10-07", -2_000).await;
    world
        .splits(
            &purchase,
            &json!([{"category": food, "minor_units": -2_000}]),
        )
        .await;
    let filed = |minor_units: i64| json!([{"category": moved, "minor_units": minor_units}]);
    world.splits(&outflow, &filed(-50_000)).await;
    world.splits(&inflow, &filed(50_000)).await;
    let unmatched = format!("{TRANSACTIONS}?unmatched_transfer=true");

    let (status, page) = world
        .send(Method::GET, &unmatched, &world.admin, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let found = ids(&page);
    assert_eq!(found.len(), 2);
    assert!(found.contains(&outflow) && found.contains(&inflow));
    assert!(!found.contains(&unfiled) && !found.contains(&purchase));

    let (_, filtered) = world
        .send(
            Method::GET,
            &format!("{unmatched}&account={}", world.savings),
            &world.admin,
            None,
        )
        .await;
    assert_eq!(ids(&filtered), std::slice::from_ref(&inflow));

    world.link(&outflow, &inflow).await;
    let (_, page) = world
        .send(Method::GET, &unmatched, &world.admin, None)
        .await;
    assert_eq!(ids(&page), [""; 0]);
}

/// A token sees the unmatched legs of the accounts it reads, and no more.
#[tokio::test]
async fn unmatched_legs_follow_the_tokens_grants() {
    let world = world().await;
    let moved = world.category("Moved", "transfer").await;
    let (outflow, inflow) = world.pair().await;
    world
        .splits(
            &outflow,
            &json!([{"category": moved, "minor_units": -50_000}]),
        )
        .await;
    world
        .splits(
            &inflow,
            &json!([{"category": moved, "minor_units": 50_000}]),
        )
        .await;
    let token = world.token(vec![on(world.checking, Transactions)]).await;

    let (_, page) = world
        .send(
            Method::GET,
            &format!("{TRANSACTIONS}?unmatched_transfer=true"),
            &token,
            None,
        )
        .await;

    assert_eq!(ids(&page), [outflow]);
}
