//! The display currency (spec §8.10 N8): set by an admin, audited, never guessed, and
//! the same for every reader.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_domain::policy::AccountLevel::{Balances, Write};
use coffer_server::ledger::settings::ForecastSettingsPatch;
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::{SETTINGS, World, all, day, on, world};

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

/// One value in a patch refused is the whole patch refused: no rebuild, no setting and no
/// log entry from the part that was good.
#[tokio::test]
async fn a_refused_mixed_patch_changes_nothing() {
    let world = world().await;
    world.golden().await;
    let (rows, settings, logged) = (
        world.rows().await,
        world.settings_as(&world.admin).await,
        world.audit_count().await,
    );

    let (status, _) = world
        .send(
            Method::PATCH,
            SETTINGS,
            &world.admin,
            Some(&json!({"display_currency": "EUR", "default_horizon_months": 61})),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(world.rows().await == rows, "nothing rebuilt");
    assert_eq!(world.settings_as(&world.admin).await, settings);
    assert_eq!(world.audit_count().await, logged);
}

/// A read sees one state of the row. Each patch here changes both values, so a read
/// holding EUR with income off, or USD with income on, saw a state that never existed.
/// A race can only pass by luck, never fail by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_never_mixes_two_patches() {
    const FLIPS: usize = 200;
    let world = world().await;
    let ledger = Arc::clone(&world.server.ledger);
    let alice = world.alice;
    let flip = |currency: &str, income: bool| -> ForecastSettingsPatch {
        serde_json::from_value(json!({"display_currency": currency, "include_income": income}))
            .unwrap()
    };
    let (eur, usd) = (flip("EUR", true), flip("USD", false));
    ledger
        .update_forecast_settings(&alice, usd.clone())
        .await
        .unwrap();

    let writer = tokio::spawn({
        let ledger = Arc::clone(&ledger);
        async move {
            for round in 0..FLIPS {
                let patch = if round % 2 == 0 { &eur } else { &usd };
                ledger
                    .update_forecast_settings(&alice, patch.clone())
                    .await
                    .unwrap();
            }
        }
    });
    while !writer.is_finished() {
        let read = ledger.household_settings(&alice).await.unwrap();
        let currency = read.display_currency.map(|known| known.as_str().to_owned());
        assert_eq!(
            currency.as_deref() == Some("EUR"),
            read.include_income,
            "a state no patch left"
        );
    }
    writer.await.unwrap();
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

/// Whether a token may set it depends on its grants alone, and what it answers on nothing
/// else held: an admin token limited to one account is refused (`grants::fx_channels` says
/// why), a token of `all` at `balances` isn't, and a token below `admin` is refused anyway.
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
        let tokens = [
            world.token(Scope::Admin, vec![on(visible, Write)]).await,
            world.token(Scope::Admin, vec![all(Balances)]).await,
            world.token(Scope::Write, vec![on(visible, Write)]).await,
        ];
        for token in &tokens {
            answers.push(
                world
                    .send(
                        Method::PATCH,
                        SETTINGS,
                        token,
                        Some(&json!({"display_currency": "JPY"})),
                    )
                    .await,
            );
        }
    }
    let (first, second) = answers.split_at(3);
    assert_eq!(first[0].0, StatusCode::FORBIDDEN);
    assert_eq!(first[1].0, StatusCode::OK);
    assert_eq!(first[2].0, StatusCode::FORBIDDEN);
    assert!(first == second, "byte-identical");
}
