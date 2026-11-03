//! Choosing the zone: who may, what is accepted and what is written.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::Scope;
use coffer_domain::time::IanaZone;
use serde_json::{Value, json};

use super::{AUCKLAND, LOS_ANGELES, LOS_ANGELES_DAY, SETTINGS, set_zone, world_at};

#[tokio::test]
async fn the_server_default_applies_until_one_is_chosen() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    let (status, text) = world.send(Method::GET, SETTINGS, &world.admin, None).await;

    assert_eq!(status, StatusCode::OK);
    let settings: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["timezone"], LOS_ANGELES);
    assert_eq!(settings["timezone_setting"], Value::Null);
}

#[tokio::test]
async fn an_admin_chooses_a_zone_and_it_is_audited_once() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    let answer = set_zone(&world, json!(AUCKLAND)).await;

    assert_eq!(answer["timezone"], AUCKLAND);
    assert_eq!(answer["timezone_setting"], AUCKLAND);
    let again = set_zone(&world, json!(AUCKLAND)).await;
    assert_eq!(
        again["timezone"], AUCKLAND,
        "saying it again changes nothing"
    );
    let events = world.settings_events().await;
    let [event] = events.as_slice() else {
        panic!("expected one event");
    };
    assert_eq!(event.action, "household.settings");
    let [(field, change)] = event.changes.iter().collect::<Vec<_>>()[..] else {
        panic!("expected one field");
    };
    assert_eq!(field, "timezone");
    assert_eq!(change.before, AuditValue::Null);
    assert_eq!(change.after, AuditValue::from(AUCKLAND));
}

#[tokio::test]
async fn clearing_returns_to_the_server_default_and_is_audited() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    set_zone(&world, json!(AUCKLAND)).await;

    let answer = set_zone(&world, Value::Null).await;

    assert_eq!(answer["timezone"], LOS_ANGELES);
    assert_eq!(answer["timezone_setting"], Value::Null);
    let events = world.settings_events().await;
    let (_, change) = events[0].changes.iter().next().unwrap();
    assert_eq!(change.before, AuditValue::from(AUCKLAND));
    assert_eq!(change.after, AuditValue::Null);
}

/// The server's own zone can be chosen too, and then outlasts a change of the variable.
#[tokio::test]
async fn choosing_the_default_by_name_is_a_choice() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    let answer = set_zone(&world, json!(LOS_ANGELES)).await;

    assert_eq!(answer["timezone_setting"], LOS_ANGELES);
    assert_eq!(world.settings_events().await.len(), 1);
}

#[tokio::test]
async fn a_name_is_stored_as_iana_spells_it() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    let answer = set_zone(&world, json!("pacific/AUCKLAND")).await;

    assert_eq!(answer["timezone_setting"], AUCKLAND);
}

#[tokio::test]
async fn anything_but_an_iana_name_is_refused_and_not_echoed() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    for name in [
        "Mars/Olympus",
        "",
        " Pacific/Auckland",
        "UTC+1",
        "+13:00",
        "localtime",
        "../../etc/passwd",
        "/usr/share/zoneinfo/UTC",
    ] {
        let (status, text) = world
            .send(
                Method::PATCH,
                SETTINGS,
                &world.admin,
                Some(&json!({"timezone": name})),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        assert!(text.contains("IANA name"), "says what it wants: {name}");
        assert!(name.is_empty() || !text.contains(name), "no echo: {name}");
    }

    let (status, _) = world
        .send(
            Method::PATCH,
            SETTINGS,
            &world.admin,
            Some(&json!({"timezone": 13})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not even a text");
    assert!(world.settings_events().await.is_empty(), "writes nothing");
}

#[tokio::test]
async fn only_an_admin_may_change_it_and_everyone_reads_it() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    set_zone(&world, json!(AUCKLAND)).await;
    let read = world.token(Scope::Read, Vec::new()).await;

    for body in [json!({"timezone": LOS_ANGELES}), json!({"timezone": null})] {
        let (status, _) = world
            .send(Method::PATCH, SETTINGS, &read, Some(&body))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    let (status, text) = world.send(Method::GET, SETTINGS, &read, None).await;
    assert_eq!(status, StatusCode::OK);
    let settings: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(settings["timezone"], AUCKLAND);
    assert_eq!(world.settings_events().await.len(), 1);
}

#[tokio::test]
async fn a_patch_leaving_it_out_keeps_it_and_it_keeps_the_rest() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;

    let first = set_zone(&world, json!(AUCKLAND)).await;
    assert_eq!(
        first["default_horizon_months"], 3,
        "the defaults, on a new row"
    );
    let answer = world
        .admin_send(Method::PATCH, SETTINGS, &json!({"stale_after_days": 9}))
        .await;
    assert_eq!(answer["timezone_setting"], AUCKLAND);
    let answer = set_zone(&world, Value::Null).await;
    assert_eq!(answer["stale_after_days"], 9);
    let answer = world
        .admin_send(Method::PATCH, SETTINGS, &json!({"display_currency": "EUR"}))
        .await;
    assert_eq!(answer["timezone_setting"], Value::Null);
    assert_eq!(answer["timezone"], LOS_ANGELES);
}

/// The page offers these names, so each has to be accepted as what it says.
#[test]
fn every_name_offered_is_accepted_as_it_is_spelled() {
    let names = IanaZone::all_names();
    assert!(names.iter().any(|name| name == AUCKLAND));
    for name in names {
        assert_eq!(IanaZone::parse(&name).unwrap().name(), name);
    }
}
