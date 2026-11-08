//! Each currency's emergency fund through the real router: set and removed by an admin,
//! audited, read only with the forecast's access, and what the forecast then says.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::secrets::SecretToken;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use serde_json::{Value, json};

use crate::forecast::{World, world};

const SETTINGS: &str = "/api/v1/household/settings";

fn uri(currency: &str) -> String {
    format!("/api/v1/household/emergency-funds/{currency}")
}

fn fixed(minor_units: i64) -> Value {
    json!({"kind": "fixed", "minor_units": minor_units})
}

async fn put(
    world: &World,
    token: &SecretToken,
    currency: &str,
    target: &Value,
) -> (StatusCode, Value) {
    let (status, text) = world
        .send(Method::PUT, &uri(currency), token, Some(target))
        .await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn delete(world: &World, token: &SecretToken, currency: &str) -> (StatusCode, Value) {
    let (status, text) = world
        .send(Method::DELETE, &uri(currency), token, None)
        .await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn settings(world: &World, token: &SecretToken) -> Value {
    let (status, text) = world.send(Method::GET, SETTINGS, token, None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_str(&text).unwrap()
}

async fn events(world: &World) -> Vec<AuditEvent> {
    world
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
        .items
}

/// An admin token whose grants stop below `all` at `transactions`.
async fn restricted_admin(world: &World) -> SecretToken {
    let grants = vec![TokenGrant {
        target: GrantTarget::All,
        level: AccountLevel::Balances,
    }];
    world.token(Scope::Admin, grants).await
}

#[tokio::test]
async fn admin_sets_and_reads_a_target() {
    let world = world().await;

    let (status, answer) = put(&world, &world.admin, "EUR", &fixed(100_000)).await;

    assert_eq!(status, StatusCode::OK);
    let expected = json!([{"currency": "EUR", "target": fixed(100_000)}]);
    assert_eq!(answer["emergency_funds"], expected, "the answer shows it");
    assert_eq!(
        settings(&world, &world.admin).await["emergency_funds"],
        expected,
        "and the settings list it"
    );
    let logged = events(&world).await;
    assert_eq!(logged.len(), 1, "one entry");
    let event = &logged[0];
    assert_eq!(event.action, "household.emergency_fund.set");
    assert_eq!(event.entity_id, "household");
    assert!(matches!(event.actor, AuditActor::Member { .. }));
    let changes: Vec<_> = event.changes.iter().collect();
    assert_eq!(changes.len(), 1);
    let (field, change) = changes[0];
    assert_eq!(field, "emergency_fund.EUR");
    assert_eq!(change.before, AuditValue::Null, "none before");
    assert_eq!(change.after, AuditValue::from("fixed:100000"));

    put(
        &world,
        &world.admin,
        "EUR",
        &json!({"kind": "months_of_outflows", "months": 3}),
    )
    .await;
    put(&world, &world.admin, "USD", &fixed(0)).await;
    assert_eq!(
        settings(&world, &world.admin).await["emergency_funds"],
        json!([
            {"currency": "EUR", "target": {"kind": "months_of_outflows", "months": 3}},
            {"currency": "USD", "target": fixed(0)},
        ]),
        "replaced, a zero kept as zero, and sorted by currency"
    );
    let replaced = &events(&world).await;
    let replacement = replaced
        .iter()
        .find_map(|event| {
            event
                .changes
                .iter()
                .find(|(_, change)| change.before == AuditValue::from("fixed:100000"))
        })
        .unwrap();
    assert_eq!(
        replacement.1.after,
        AuditValue::from("months_of_outflows:3")
    );
}

#[tokio::test]
async fn setting_the_same_target_writes_nothing() {
    let world = world().await;
    put(&world, &world.admin, "USD", &fixed(500)).await;

    let (status, answer) = put(&world, &world.admin, "USD", &fixed(500)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        answer["emergency_funds"],
        json!([{"currency": "USD", "target": fixed(500)}])
    );
    assert_eq!(events(&world).await.len(), 1, "no second entry");
}

#[tokio::test]
async fn admin_removes_a_target() {
    let world = world().await;
    put(&world, &world.admin, "USD", &fixed(500)).await;

    let (status, answer) = delete(&world, &world.admin, "USD").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["emergency_funds"], json!([]), "none left");
    let logged = events(&world).await;
    assert_eq!(logged.len(), 2);
    let removal = logged
        .iter()
        .find(|event| event.action == "household.emergency_fund.remove")
        .unwrap();
    let (field, change) = removal.changes.iter().next().unwrap();
    assert_eq!(field, "emergency_fund.USD");
    assert_eq!(change.before, AuditValue::from("fixed:500"));
    assert_eq!(change.after, AuditValue::Null);
}

#[tokio::test]
async fn removing_a_missing_target_is_not_found() {
    let world = world().await;
    put(&world, &world.admin, "USD", &fixed(500)).await;

    let (status, _) = delete(&world, &world.admin, "EUR").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(events(&world).await.len(), 1, "nothing audited");
}

#[tokio::test]
async fn a_member_cannot_set_one() {
    let world = world().await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let bob = world
        .server
        .authenticator()
        .enroll(&invite, "bob", "Bob")
        .await;
    let bob = Actor {
        member: bob,
        role: Role::Member,
        via: Via::Session,
    };
    let (_, bob_write) = world
        .server
        .agent_tokens
        .create(
            &bob,
            TokenName::parse("bob's").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let write = world.token(Scope::Write, Vec::new()).await;

    for token in [&bob_write, &write] {
        let (status, _) = put(&world, token, "USD", &fixed(500)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "it needs admin");
        let (status, _) = delete(&world, token, "USD").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "removing too");
    }
    assert!(events(&world).await.is_empty(), "nothing written");
}

/// A token below the forecast's access never learns a target, whether one is set or not: its
/// read, and its own change's answer, say `null`.
#[tokio::test]
async fn a_restricted_token_reads_no_emergency_fund() {
    let world = world().await;
    let restricted = restricted_admin(&world).await;
    assert_eq!(
        settings(&world, &restricted).await["emergency_funds"],
        Value::Null,
        "none set"
    );

    let (status, answer) = put(&world, &restricted, "USD", &fixed(700)).await;
    assert_eq!(status, StatusCode::OK, "an admin may set one");
    assert_eq!(
        answer["emergency_funds"],
        Value::Null,
        "the answer hides it"
    );

    let (status, answer) = put(&world, &restricted, "EUR", &fixed(900)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        answer["emergency_funds"],
        Value::Null,
        "with one already set too"
    );
    assert_eq!(
        settings(&world, &restricted).await["emergency_funds"],
        Value::Null,
        "and so does a read"
    );
    assert_eq!(
        settings(&world, &world.admin).await["emergency_funds"],
        json!([
            {"currency": "EUR", "target": fixed(900)},
            {"currency": "USD", "target": fixed(700)},
        ]),
        "both were kept"
    );
}

/// Whether a target is set is what removing one would tell, so a token below the forecast's
/// access is refused alike whatever is set.
#[tokio::test]
async fn a_restricted_token_cannot_remove_one() {
    let world = world().await;
    let restricted = restricted_admin(&world).await;
    put(&world, &world.admin, "USD", &fixed(700)).await;

    let set = delete(&world, &restricted, "USD").await;
    let missing = delete(&world, &restricted, "EUR").await;

    assert_eq!(set.0, StatusCode::FORBIDDEN);
    assert_eq!(set, missing, "the same refusal");
    assert_eq!(events(&world).await.len(), 1, "nothing removed");
}

#[tokio::test]
async fn out_of_range_is_422() {
    let world = world().await;
    for target in [
        fixed(-1),
        fixed(1_000_000_000_000_001),
        json!({"kind": "months_of_outflows", "months": 0}),
        json!({"kind": "months_of_outflows", "months": 61}),
    ] {
        let (status, _) = put(&world, &world.admin, "USD", &target).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{target}");
    }
    for target in [
        json!({"kind": "months_of_outflows", "months": 300}),
        json!({"kind": "percent", "months": 3}),
        json!({"kind": "fixed", "minor_units": 1, "currency": "USD"}),
    ] {
        let (status, _) = put(&world, &world.admin, "USD", &target).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{target}");
    }
    assert!(events(&world).await.is_empty(), "nothing written");
}

#[tokio::test]
async fn a_bad_currency_in_the_path_is_400() {
    let world = world().await;
    for currency in ["usd", "US", "USDX", "ZZZ", "%3Cb%3E"] {
        let (status, body) = put(&world, &world.admin, currency, &fixed(1)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{currency}");
        assert!(!body.to_string().contains("<b>"), "never echoed");
    }
    assert!(events(&world).await.is_empty(), "nothing written");
}

#[tokio::test]
async fn the_forecast_uses_it() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    let without = world.forecast().await;
    assert_eq!(
        without["cautions"],
        json!([{"kind": "emergency_fund_not_set", "currency": "USD"}])
    );

    put(&world, &world.admin, "USD", &fixed(250_000)).await;

    let forecast = world.forecast().await;
    assert_eq!(forecast["cautions"], json!([]), "no caution for it");
    assert_eq!(
        forecast["totals"][0]["emergency_fund"],
        json!({"minor_units": 250_000, "currency": "USD"})
    );
    assert_eq!(
        forecast["surplus"],
        json!({"minor_units": 750_000, "currency": "USD"})
    );
    assert_eq!(
        forecast["emergency_funds"],
        json!([{"currency": "USD", "target": fixed(250_000)}])
    );
}
