//! Who may touch a link: `write` on both accounts, and a token that can't see one side learns
//! nothing of it (spec §8.2, §8.12 T3).

use axum::http::{Method, StatusCode};
use coffer_domain::ids::TransactionId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use serde_json::Value;

use super::{LINKS, TRANSACTIONS, linking, on, world};

/// A token that can't see one side gets the very answer a missing transaction gets.
#[tokio::test]
async fn a_hidden_side_answers_as_missing() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let token = world.token(vec![on(world.checking, Write)]).await;

    let hidden = world
        .send(
            Method::POST,
            LINKS,
            &token,
            Some(&linking(&outflow, &inflow)),
        )
        .await;
    let missing = world
        .send(
            Method::POST,
            LINKS,
            &token,
            Some(&linking(&outflow, &TransactionId::new().to_string())),
        )
        .await;

    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(hidden, missing);
}

/// Seeing a side isn't writing to it.
#[tokio::test]
async fn a_link_needs_write_on_both_accounts() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let token = world
        .token(vec![
            on(world.checking, Write),
            on(world.savings, Transactions),
        ])
        .await;

    let (status, _) = world
        .send(
            Method::POST,
            LINKS,
            &token,
            Some(&linking(&outflow, &inflow)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let link = world.link(&outflow, &inflow).await;
    let (status, _) = world
        .send(
            Method::DELETE,
            &format!("{LINKS}/{}", link["id"].as_str().unwrap()),
            &token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, read) = world
        .send(
            Method::GET,
            &format!("{LINKS}/{}", link["id"].as_str().unwrap()),
            &token,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "reading needs only transactions");
    assert_eq!(read, link);
}

/// With one side hidden, a link doesn't exist: not on the transaction, not by its id, and not
/// as something to remove.
#[tokio::test]
async fn a_link_to_a_hidden_side_doesnt_show() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    let link = world.link(&outflow, &inflow).await;
    let uri = format!("{LINKS}/{}", link["id"].as_str().unwrap());
    let token = world.token(vec![on(world.checking, Write)]).await;

    for tiers in [
        vec![on(world.checking, Write)],
        vec![on(world.checking, Write), on(world.savings, Balances)],
    ] {
        let token = world.token(tiers).await;
        let (status, read) = world
            .send(
                Method::GET,
                &format!("{TRANSACTIONS}/{outflow}"),
                &token,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["transfer_link"], Value::Null);
    }

    let (status, _) = world.send(Method::GET, &uri, &token, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = world.send(Method::DELETE, &uri, &token, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A token that doesn't see the link is told only that the change can't be made, and in no
/// words of a link: its refusal and the admin's are worded apart.
#[tokio::test]
async fn a_refusal_doesnt_name_a_hidden_link() {
    let world = world().await;
    let (outflow, inflow) = world.pair().await;
    world.link(&outflow, &inflow).await;
    let food = world.category("Food", "expense").await;
    let token = world.token(vec![on(world.checking, Write)]).await;
    let two = serde_json::json!({"splits": [
        {"category": food, "minor_units": -20_000},
        {"category": food, "minor_units": -30_000},
    ]});
    let uri = format!("{TRANSACTIONS}/{outflow}");

    for (method, body) in [(Method::PATCH, Some(&two)), (Method::DELETE, None)] {
        let (status, refusal) = world.send(method, &uri, &token, body).await;
        assert!(status.is_client_error());
        assert!(!refusal["detail"].as_str().unwrap().contains("link"));
    }
    let (_, refusal) = world
        .send(Method::PATCH, &uri, &world.admin, Some(&two))
        .await;
    assert!(refusal["detail"].as_str().unwrap().contains("link"));
}
