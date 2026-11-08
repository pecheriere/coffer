//! The household page's forecast settings card.

use axum::http::{StatusCode, header};

use super::{HOUSEHOLD, alice, bob, household_events, post, settings};
use crate::forecast::world;
use crate::pages::forecast::section;

const SAVE: &str = "/admin/household/forecast";

#[tokio::test]
async fn the_card_shows_the_current_settings() {
    let world = world().await;
    world
        .settings(&serde_json::json!({
            "include_income": true,
            "default_horizon_months": 9,
            "stale_after_days": 12,
        }))
        .await;
    let mut admin = alice(&world).await;

    let body = admin.get(HOUSEHOLD).await.body;
    let card = section(&body, "forecast");

    assert!(card.contains("<h2>Cash forecast</h2>"));
    assert!(card.contains(&format!("action=\"{SAVE}\"")));
    assert!(card.contains("name=\"include_income\" checked"));
    assert!(card.contains("name=\"horizon\" value=\"9\""));
    assert!(card.contains("name=\"stale_after_days\" value=\"12\""));
}

#[tokio::test]
async fn admin_saves_forecast_settings() {
    let world = world().await;
    world
        .settings(&serde_json::json!({"include_income": false}))
        .await;
    let before = household_events(&world).await.len();
    let mut admin = alice(&world).await;

    let reply = post(
        &mut admin,
        SAVE,
        &[
            ("include_income", "on"),
            ("horizon", "6"),
            ("stale_after_days", "10"),
        ],
    )
    .await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), "/admin/household#forecast");
    let saved = settings(&world).await;
    assert!(saved.include_income);
    assert_eq!(saved.default_horizon_months, 6);
    assert_eq!(saved.stale_after_days, 10);
    assert_eq!(household_events(&world).await.len(), before + 1);
}

#[tokio::test]
async fn unticking_income_turns_it_off() {
    let world = world().await;
    world
        .settings(&serde_json::json!({"include_income": true}))
        .await;
    let mut admin = alice(&world).await;

    let reply = post(
        &mut admin,
        SAVE,
        &[("horizon", "6"), ("stale_after_days", "10")],
    )
    .await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert!(!settings(&world).await.include_income);
}

#[tokio::test]
async fn out_of_range_numbers_are_refused() {
    let world = world().await;
    let mut admin = alice(&world).await;
    let unchanged = settings(&world).await;
    let before = household_events(&world).await.len();

    for (horizon, stale) in [
        ("0", "10"),
        ("61", "10"),
        ("6", "0"),
        ("6", "366"),
        ("", "10"),
        ("6", "ten"),
    ] {
        let reply = post(
            &mut admin,
            SAVE,
            &[
                ("include_income", "on"),
                ("horizon", horizon),
                ("stale_after_days", stale),
            ],
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{horizon} {stale}"
        );
    }
    let reply = post(
        &mut admin,
        SAVE,
        &[
            ("include_income", "yes please"),
            ("horizon", "6"),
            ("stale_after_days", "10"),
        ],
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(settings(&world).await, unchanged);
    assert_eq!(household_events(&world).await.len(), before);
}

#[tokio::test]
async fn a_member_or_a_missing_token_is_refused() {
    let world = world().await;
    let unchanged = settings(&world).await;
    let fields = [
        ("include_income", "on"),
        ("horizon", "7"),
        ("stale_after_days", "11"),
    ];
    let mut member = bob(&world).await;
    let mut admin = alice(&world).await;

    let by_member = post(&mut member, SAVE, &fields).await;
    let without_token = admin.post_form(SAVE, &fields).await;

    assert_eq!(by_member.status, StatusCode::FORBIDDEN);
    assert_eq!(without_token.status, StatusCode::FORBIDDEN);
    assert_eq!(settings(&world).await, unchanged);
}
