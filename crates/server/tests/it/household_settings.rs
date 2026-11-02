//! The household forecast settings through the real router with agent
//! tokens.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::policy::{AccountLevel, DenyReason, GrantTarget, TokenGrant};
use coffer_server::auth::AuthError;
use coffer_server::http::router;
use coffer_server::ledger::LedgerError;
use coffer_server::ledger::settings::ForecastSettingsPatch;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const URI: &str = "/api/v1/household/settings";

struct World {
    server: TestServer,
    alice: Actor,
    bob: Actor,
}

async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let alice = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = actor(alice, Role::Admin);
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let bob = server.authenticator().enroll(&invite, "bob", "Bob").await;
    World {
        server,
        alice,
        bob: actor(bob, Role::Member),
    }
}

fn actor(member: MemberId, role: Role) -> Actor {
    Actor {
        member,
        role,
        via: Via::Session,
    }
}

async fn token(world: &World, owner: &Actor, scope: Scope) -> (AgentTokenId, SecretToken) {
    let (record, secret) = world
        .server
        .agent_tokens
        .create(
            owner,
            TokenName::parse("claude").unwrap(),
            scope,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    (record.id, secret)
}

async fn call(
    server: &TestServer,
    method: Method,
    token: &SecretToken,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(URI).header(
        header::AUTHORIZATION,
        format!("Bearer {}", token.expose_secret()),
    );
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

async fn patch(server: &TestServer, token: &SecretToken, body: Value) -> (StatusCode, Value) {
    call(server, Method::PATCH, token, Some(body)).await
}

async fn get(server: &TestServer, token: &SecretToken) -> Value {
    let (status, body) = call(server, Method::GET, token, None).await;
    assert_eq!(status, StatusCode::OK, "reading settings answers 200");
    body
}

async fn events(server: &TestServer) -> Vec<AuditEvent> {
    server
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

fn fixed(minor_units: i64) -> Value {
    json!({"kind": "fixed", "amount": {"minor_units": minor_units, "currency": "USD"}})
}

#[tokio::test]
async fn unset_settings_read_as_defaults() {
    let world = world().await;
    let (_, read) = token(&world, &world.alice, Scope::Read).await;

    let settings = get(&world.server, &read).await;

    assert_eq!(
        settings,
        json!({"floor": null, "include_income": false,
               "default_horizon_months": 3, "stale_after_days": 7}),
        "no row reads as the defaults, with the floor present and null"
    );
}

#[tokio::test]
async fn admin_token_sets_a_fixed_floor_and_it_is_audited() {
    let world = world().await;
    let (agent, admin) = token(&world, &world.alice, Scope::Admin).await;
    let (_, read) = token(&world, &world.alice, Scope::Read).await;

    let (status, answer) = patch(
        &world.server,
        &admin,
        json!({"floor": fixed(500_000), "include_income": true,
               "default_horizon_months": 6, "stale_after_days": 14}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "an admin token may change settings");
    let expected = json!({"floor": fixed(500_000), "include_income": true,
                          "default_horizon_months": 6, "stale_after_days": 14});
    assert_eq!(
        answer, expected,
        "the answer is the settings as they now are"
    );
    assert_eq!(
        get(&world.server, &read).await,
        expected,
        "a read token sees the stored settings"
    );
    let logged = events(&world.server).await;
    assert_eq!(logged.len(), 1, "one event for the whole patch");
    let event = &logged[0];
    assert_eq!(event.action, "household.settings", "the action");
    assert_eq!(event.entity_id, "household", "the entity id");
    assert_eq!(
        event.actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: Some(agent)
        },
        "the member and the agent are attributed"
    );
    let fields: Vec<_> = event.changes.iter().map(|(field, _)| field).collect();
    assert_eq!(
        fields,
        [
            "default_horizon_months",
            "floor",
            "include_income",
            "stale_after_days"
        ],
        "one field per changed setting"
    );
    let floor = event.changes.iter().find(|(f, _)| *f == "floor").unwrap().1;
    assert_eq!(floor.before, AuditValue::Null, "no floor before");
    assert_eq!(
        floor.after,
        AuditValue::from("fixed:USD:500000"),
        "the floor is recorded in canonical form"
    );
    let horizon = event
        .changes
        .iter()
        .find(|(f, _)| *f == "default_horizon_months")
        .unwrap()
        .1;
    assert_eq!(horizon.before, AuditValue::Int(3), "horizon before");
    assert_eq!(horizon.after, AuditValue::Int(6), "horizon after");
}

#[tokio::test]
async fn explicit_zero_floor_differs_from_an_unset_floor() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;

    let (status, answer) = patch(&world.server, &admin, json!({"floor": fixed(0)})).await;

    assert_eq!(status, StatusCode::OK, "a floor of 0 is allowed");
    assert_eq!(answer["floor"], fixed(0), "a stored 0 is not null");
    let logged = events(&world.server).await;
    assert_eq!(logged.len(), 1, "setting 0 over no floor is a change");
}

#[tokio::test]
async fn write_token_is_refused() {
    let world = world().await;
    let (_, write) = token(&world, &world.alice, Scope::Write).await;

    let (status, body) = patch(&world.server, &write, json!({"include_income": true})).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "write is below admin");
    assert_eq!(
        body["detail"], "this action requires admin rights",
        "the refusal names the right"
    );
    assert!(
        events(&world.server).await.is_empty(),
        "a refused change writes nothing"
    );
    let (status, _) = call(&world.server, Method::GET, &write, None).await;
    assert_eq!(status, StatusCode::OK, "a write token may still read");
}

#[tokio::test]
async fn member_role_cannot_exceed_its_ceiling() {
    let world = world().await;

    let err = world
        .server
        .agent_tokens
        .create(
            &world.bob,
            TokenName::parse("too much").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, AuthError::Denied(denied) if denied.reason == DenyReason::ScopeAboveRole),
        "a member cannot mint an admin token"
    );
    let (_, bob_write) = token(&world, &world.bob, Scope::Write).await;
    let (status, _) = patch(&world.server, &bob_write, json!({"include_income": true})).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the member's strongest token cannot change settings"
    );
    let (_, bob_read) = token(&world, &world.bob, Scope::Read).await;
    assert_eq!(
        get(&world.server, &bob_read).await["default_horizon_months"],
        3,
        "a member may read"
    );
}

#[tokio::test]
async fn invalid_values_are_422() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;
    let invalid = [
        json!({"floor": fixed(-1)}),
        json!({"floor": fixed(1_000_000_000_000_001_i64)}),
        json!({"floor": {"kind": "months_of_outflows", "months": 0}}),
        json!({"floor": {"kind": "months_of_outflows", "months": 61}}),
        json!({"default_horizon_months": 0}),
        json!({"default_horizon_months": 61}),
        json!({"stale_after_days": 0}),
        json!({"stale_after_days": 366}),
    ];

    for body in invalid {
        let (status, _) = patch(&world.server, &admin, body).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "an out-of-range value is 422"
        );
    }
    for body in [
        json!({"floor": {"kind": "percent", "months": 3}}),
        json!({"timezone": "UTC"}),
    ] {
        let (status, _) = patch(&world.server, &admin, body).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "an unknown kind or field is 400"
        );
    }
    assert!(
        events(&world.server).await.is_empty(),
        "refused values write nothing"
    );
    let (_, read) = token(&world, &world.alice, Scope::Read).await;
    assert_eq!(
        get(&world.server, &read).await["floor"],
        Value::Null,
        "nothing was stored"
    );
}

#[tokio::test]
async fn boundary_values_are_accepted() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;

    let (status, answer) = patch(
        &world.server,
        &admin,
        json!({"floor": {"kind": "months_of_outflows", "months": 60},
               "default_horizon_months": 60, "stale_after_days": 365}),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "the upper bounds are valid");
    assert_eq!(answer["stale_after_days"], 365, "stale bound");
    let (status, answer) = patch(
        &world.server,
        &admin,
        json!({"floor": {"kind": "months_of_outflows", "months": 1},
               "default_horizon_months": 1, "stale_after_days": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the lower bounds are valid");
    assert_eq!(answer["floor"]["months"], 1, "floor months bound");
}

#[tokio::test]
async fn noop_patch_writes_no_audit() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;
    let body = json!({"floor": fixed(100), "include_income": true});
    patch(&world.server, &admin, body.clone()).await;
    assert_eq!(events(&world.server).await.len(), 1, "the first patch");

    let (status, _) = patch(&world.server, &admin, body).await;
    assert_eq!(status, StatusCode::OK, "a repeat is accepted");
    let (status, _) = patch(&world.server, &admin, json!({})).await;
    assert_eq!(status, StatusCode::OK, "an empty patch is accepted");
    let (status, _) = patch(&world.server, &admin, json!({"stale_after_days": 7})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a default value over no row is fine"
    );

    assert_eq!(
        events(&world.server).await.len(),
        1,
        "patches that change nothing write no audit"
    );
}

#[tokio::test]
async fn floor_can_be_replaced_not_unset() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;
    patch(&world.server, &admin, json!({"floor": fixed(100)})).await;

    let (status, answer) = patch(
        &world.server,
        &admin,
        json!({"floor": {"kind": "months_of_outflows", "months": 4}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a floor may be replaced");
    assert_eq!(
        answer["floor"],
        json!({"kind": "months_of_outflows", "months": 4}),
        "the new floor"
    );
    let logged = events(&world.server).await;
    let replaced = logged
        .iter()
        .find(|event| {
            event
                .changes
                .iter()
                .any(|(_, c)| c.before == AuditValue::from("fixed:USD:100"))
        })
        .unwrap();
    let floor = replaced.changes.iter().next().unwrap().1;
    assert_eq!(
        floor.after,
        AuditValue::from("months_of_outflows:4"),
        "the replacement is audited before and after"
    );

    let (status, answer) = patch(&world.server, &admin, json!({"floor": null})).await;
    assert_eq!(status, StatusCode::OK, "null leaves the floor");
    assert_eq!(
        answer["floor"],
        json!({"kind": "months_of_outflows", "months": 4}),
        "a null floor changes nothing"
    );
    assert_eq!(events(&world.server).await.len(), 2, "no event for null");
}

#[tokio::test]
async fn numbers_outside_the_field_type_are_400() {
    let world = world().await;
    let (_, admin) = token(&world, &world.alice, Scope::Admin).await;

    for body in [
        json!({"default_horizon_months": 300}),
        json!({"floor": {"kind": "months_of_outflows", "months": 300}}),
        json!({"stale_after_days": 70_000}),
        json!({"default_horizon_months": -1}),
    ] {
        let (status, _) = patch(&world.server, &admin, body).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a number outside the field's type is 400"
        );
    }
    assert!(
        events(&world.server).await.is_empty(),
        "refused values write nothing"
    );
}

async fn granted_token(world: &World, scope: Scope) -> SecretToken {
    let grants = vec![TokenGrant {
        target: GrantTarget::All,
        level: AccountLevel::Balances,
    }];
    let (_, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("restricted").unwrap(),
            scope,
            None,
            grants,
        )
        .await
        .unwrap();
    secret
}

#[tokio::test]
async fn account_grants_do_not_change_settings_access() {
    let world = world().await;
    let read = granted_token(&world, Scope::Read).await;
    let (status, _) = call(&world.server, Method::GET, &read, None).await;
    assert_eq!(status, StatusCode::OK, "a granted read token may read");

    let write = granted_token(&world, Scope::Write).await;
    let (status, _) = patch(&world.server, &write, json!({"include_income": true})).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a granted write token may not change settings"
    );
    assert!(events(&world.server).await.is_empty(), "nothing written");
}

#[tokio::test]
async fn member_session_reads_but_cannot_change() {
    let world = world().await;

    let settings = world.server.ledger.forecast_settings(&world.bob).await;
    assert_eq!(
        settings.unwrap(),
        coffer_domain::plan::ForecastSettings::default(),
        "a member session reads"
    );
    let patch = ForecastSettingsPatch {
        include_income: Some(true),
        ..ForecastSettingsPatch::default()
    };
    let err = world
        .server
        .ledger
        .update_forecast_settings(&world.bob, patch)
        .await
        .unwrap_err();
    assert!(
        matches!(err, LedgerError::Denied(_)),
        "a member session may not change settings"
    );
    assert!(events(&world.server).await.is_empty(), "nothing written");
}
