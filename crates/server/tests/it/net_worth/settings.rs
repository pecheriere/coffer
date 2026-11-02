//! The display currency (spec §8.10 N8): set by an admin, audited, never guessed, and
//! the same for every reader.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::policy::AccountLevel::Write;
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{SETTINGS, World, day, on, world};

impl World {
    async fn settings_as(&self, token: &SecretToken) -> Value {
        let (status, text) = self.send(Method::GET, SETTINGS, token, None).await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }
}

#[tokio::test]
async fn display_currency_starts_null() {
    let world = world().await;
    world.holding("Checking", "USD").await;

    let settings = world.settings_as(&world.admin).await;

    assert_eq!(settings["display_currency"], Value::Null, "never guessed");
    assert!(
        settings
            .as_object()
            .unwrap()
            .contains_key("display_currency"),
        "present, not left out"
    );
}

#[tokio::test]
async fn admin_sets_it_and_it_is_audited() {
    let world = world().await;

    let answer = world
        .admin_send(Method::PATCH, SETTINGS, &json!({"display_currency": "EUR"}))
        .await;

    assert_eq!(answer["display_currency"], "EUR");
    assert_eq!(
        world.settings_as(&world.admin).await["display_currency"],
        "EUR"
    );
    let logged = world.settings_events().await;
    let [event] = logged.as_slice() else {
        panic!("expected one event");
    };
    assert_eq!(event.action, "household.settings");
    assert_eq!(
        event.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(world.admin_id),
        }
    );
    let [(field, change)] = event.changes.iter().collect::<Vec<_>>()[..] else {
        panic!("expected one field");
    };
    assert_eq!(field, "display_currency");
    assert_eq!(change.before, AuditValue::Null);
    assert_eq!(change.after, AuditValue::from("EUR"));

    world.display("USD").await;
    let logged = world.settings_events().await;
    let newest = logged
        .iter()
        .find(|event| {
            event
                .changes
                .iter()
                .any(|(_, change)| change.after == AuditValue::from("USD"))
        })
        .unwrap_or_else(|| panic!("the replacement is logged"));
    let (_, change) = newest.changes.iter().next().unwrap();
    assert_eq!(change.before, AuditValue::from("EUR"));
}

#[tokio::test]
async fn it_rewrites_the_history_in_its_own_request() {
    let world = world().await;
    let golden = world.golden().await;

    world.display("EUR").await;

    let rows = world.rows().await;
    let row = rows
        .iter()
        .find(|row| row.account == golden.checking && row.on == day("2026-10-30"));
    assert_eq!(row.unwrap().display_minor, Some(492_302));
}

#[tokio::test]
async fn write_token_is_refused() {
    let world = world().await;
    let token = world.token(Scope::Write, Vec::new()).await;

    let (status, _) = world
        .send(
            Method::PATCH,
            SETTINGS,
            &token,
            Some(&json!({"display_currency": "EUR"})),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        world.settings_as(&world.admin).await["display_currency"],
        Value::Null
    );
    assert!(world.settings_events().await.is_empty(), "nothing logged");
}

#[tokio::test]
async fn null_or_absent_keeps_it() {
    let world = world().await;
    world.display("EUR").await;

    for patch in [
        json!({"display_currency": null}),
        json!({}),
        json!({"include_income": true}),
    ] {
        let answer = world.admin_send(Method::PATCH, SETTINGS, &patch).await;
        assert_eq!(answer["display_currency"], "EUR");
    }
}

#[tokio::test]
async fn unknown_code_is_400_without_echo() {
    let world = world().await;

    for sent in ["ZZQ", "usd", "DOLLARS"] {
        let (status, text) = world
            .send(
                Method::PATCH,
                SETTINGS,
                &world.admin,
                Some(&json!({"display_currency": sent})),
            )
            .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!text.contains(sent), "the refusal does not echo");
    }
    assert!(world.settings_events().await.is_empty(), "nothing logged");
}

#[tokio::test]
async fn same_value_writes_nothing() {
    let world = world().await;
    world.golden().await;
    let rows = world.rows().await;
    let logged = world.settings_events().await.len();

    world.display("USD").await;

    assert_eq!(world.settings_events().await.len(), logged);
    assert!(world.rows().await == rows, "nothing rebuilt");
}

/// A patch of another field on its own rebuilds nothing.
#[tokio::test]
async fn another_field_rebuilds_nothing() {
    let world = world().await;
    world.golden().await;
    let rows = world.rows().await;

    world
        .admin_send(Method::PATCH, SETTINGS, &json!({"stale_after_days": 9}))
        .await;

    assert!(world.rows().await == rows, "nothing rebuilt");
    assert_eq!(
        world.settings_as(&world.admin).await["display_currency"],
        "USD"
    );
}

#[tokio::test]
async fn forecast_fields_are_unchanged() {
    let world = world().await;

    world
        .admin_send(
            Method::PATCH,
            SETTINGS,
            &json!({"display_currency": "EUR", "include_income": true,
                    "default_horizon_months": 6, "stale_after_days": 14}),
        )
        .await;

    assert_eq!(
        world.settings_as(&world.admin).await,
        json!({"floor": null, "include_income": true, "default_horizon_months": 6,
               "stale_after_days": 14, "display_currency": "EUR"})
    );
}

/// It says how numbers are shown, not what the household holds.
#[tokio::test]
async fn it_reads_the_same_for_every_token() {
    let world = world().await;
    let visible = world.holding("Visible", "USD").await;
    world.display("JPY").await;

    for token in [
        world.token(Scope::Read, Vec::new()).await,
        world.sees_only(visible).await,
    ] {
        assert_eq!(world.settings_as(&token).await["display_currency"], "JPY");
    }
}

/// An admin token limited to one account may still set it (it is `admin`, whatever the
/// grants), and neither whether it may nor what it answers depends on what else is held.
#[tokio::test]
async fn setting_it_says_nothing_of_hidden_accounts() {
    let bare = world().await;
    let held = world().await;
    let mut answers = Vec::new();
    for (world, hide) in [(&bare, false), (&held, true)] {
        let visible = world.holding("Visible", "USD").await;
        if hide {
            let hidden = world.holding("Hidden", "JPY").await;
            world.balance(hidden, "2026-10-26", 7_777_777).await;
        }
        let token = world.token(Scope::Admin, vec![on(visible, Write)]).await;
        for code in ["JPY", "THB"] {
            answers.push(
                world
                    .send(
                        Method::PATCH,
                        SETTINGS,
                        &token,
                        Some(&json!({"display_currency": code})),
                    )
                    .await,
            );
        }
        let refused = world.token(Scope::Write, vec![on(visible, Write)]).await;
        answers.push(
            world
                .send(
                    Method::PATCH,
                    SETTINGS,
                    &refused,
                    Some(&json!({"display_currency": "JPY"})),
                )
                .await,
        );
    }
    let (first, second) = answers.split_at(3);
    assert_eq!(first[0].0, StatusCode::OK);
    assert_eq!(first[2].0, StatusCode::FORBIDDEN);
    assert!(first == second, "byte-identical");
}
