//! The tool that sets an emergency fund: audited to its token, refused as its API operation
//! refuses, and a refused argument stores nothing.

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use coffer_store::{AuditFilter, PageRequest};
use serde_json::{Value, json};

use super::{Answer, tool};
use crate::forecast::{World, world};

const SETTINGS: &str = "/api/v1/household/settings";

/// The emergency funds as alice's admin token reads them.
async fn stored(world: &World) -> Value {
    let (status, text) = world.send(Method::GET, SETTINGS, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_str::<Value>(&text).unwrap()["emergency_funds"].clone()
}

#[tokio::test]
async fn setting_an_emergency_fund_is_audited_to_its_token() {
    let world = world().await;

    let answer = tool(
        &world.server,
        &world.admin,
        "set_emergency_fund",
        json!({"currency": "USD", "target": {"kind": "fixed", "minor_units": 250_000}}),
    )
    .await;

    let expected =
        json!([{"currency": "USD", "target": {"kind": "fixed", "minor_units": 250_000}}]);
    let Answer::Value(settings) = answer else {
        panic!("an admin token may set one");
    };
    assert_eq!(settings["emergency_funds"], expected);
    assert_eq!(stored(&world).await, expected, "it was stored");
    let events = world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("household".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items;
    let [event] = events.as_slice() else {
        panic!("one entry");
    };
    assert_eq!(event.action, "household.emergency_fund.set");
    assert!(matches!(
        event.actor,
        AuditActor::Member { agent: Some(_), .. }
    ));
    let (field, change) = event.changes.iter().next().unwrap();
    assert_eq!(field, "emergency_fund.USD");
    assert_eq!(change.after, AuditValue::from("fixed:250000"));
}

/// A write token is shown the tool, since it writes, and the handler refuses it as the API
/// does: an emergency fund is an admin's.
#[tokio::test]
async fn a_write_token_setting_an_emergency_fund_is_refused_and_nothing_changes() {
    let world = world().await;
    let write = world.token(Scope::Write, Vec::new()).await;
    let target = json!({"kind": "fixed", "minor_units": 1});
    let (status, text) = world
        .send(
            Method::PUT,
            "/api/v1/household/emergency-funds/USD",
            &write,
            Some(&target),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let refused: Value = serde_json::from_str(&text).unwrap();

    let answer = tool(
        &world.server,
        &write,
        "set_emergency_fund",
        json!({"currency": "USD", "target": target}),
    )
    .await;

    assert_eq!(answer, Answer::Problem(refused));
    assert_eq!(stored(&world).await, json!([]), "nothing was set");
}

#[tokio::test]
async fn a_refused_emergency_fund_is_named_and_not_stored() {
    let world = world().await;

    for arguments in [
        json!({"currency": "usd", "target": {"kind": "fixed", "minor_units": 1}}),
        json!({"currency": "USD", "target": {"kind": "months_of_outflows", "months": 61}}),
        json!({"currency": "USD"}),
    ] {
        let answer = tool(
            &world.server,
            &world.admin,
            "set_emergency_fund",
            arguments.clone(),
        )
        .await;
        assert!(
            matches!(answer, Answer::Problem(ref problem) if problem["code"] == "invalid"),
            "{arguments}"
        );
    }
    assert_eq!(stored(&world).await, json!([]), "nothing was set");
}
