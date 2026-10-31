//! Accounts, balance snapshots and idempotent creates, through the real
//! router with agent tokens.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::AgentTokenId;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
}

/// Admin "alice", signed in with a passkey.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    World {
        server,
        alice: Actor {
            member,
            role: Role::Admin,
            via: Via::Session,
        },
    }
}

async fn token(world: &World, scope: Scope) -> (AgentTokenId, SecretToken) {
    let (record, secret) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            scope,
            None,
        )
        .await
        .unwrap();
    (record.id, secret)
}

fn request(
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<&str>,
    key: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri).header(
        header::AUTHORIZATION,
        format!("Bearer {}", token.expose_secret()),
    );
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

async fn send(server: &TestServer, request: Request<Body>) -> (StatusCode, Value) {
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

async fn call(
    server: &TestServer,
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map(|body| body.to_string());
    send(server, request(method, uri, token, body.as_deref(), None)).await
}

async fn create(server: &TestServer, token: &SecretToken, body: Value) -> Value {
    let (status, created) = call(server, Method::POST, "/api/v1/accounts", token, Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

async fn account_count(server: &TestServer) -> usize {
    server
        .store
        .list_accounts(&coffer_domain::policy::AccountScope::All, true)
        .await
        .unwrap()
        .len()
}

async fn audit_actions(server: &TestServer, action: &str) -> Vec<coffer_store::AuditEvent> {
    server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("account".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|event| event.action == action)
        .collect()
}

#[tokio::test]
async fn create_manual_account_defaults_liquidity() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    let created = create(
        &world.server,
        &write,
        json!({"name": "Visa", "kind": "credit_card", "currency": "USD"}),
    )
    .await;

    assert_eq!(created["name"], "Visa");
    assert_eq!(created["kind"], "credit_card");
    assert_eq!(created["currency"], "USD");
    assert_eq!(created["liquidity"], "short_term_liability");
    assert_eq!(created["liquidity_overridden"], false);
    assert_eq!(created["source"], "manual");
    assert_eq!(created["include_in_net_worth"], true);
    assert_eq!(created["include_in_forecast"], true);
    assert_eq!(created["closed"], false);
    assert!(created["latest_balance"].is_null());

    let id = created["id"].as_str().unwrap();
    let (status, fetched) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}"),
        &write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, created);
}

#[tokio::test]
async fn chosen_liquidity_is_kept_as_an_override() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Emergency fund", "kind": "brokerage", "currency": "EUR",
               "liquidity": "cash", "include_in_forecast": false}),
    )
    .await;
    assert_eq!(created["liquidity"], "cash");
    assert_eq!(created["liquidity_overridden"], true);
    assert_eq!(created["include_in_forecast"], false);
    let id = created["id"].as_str().unwrap();

    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"kind": "retirement"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["kind"], "retirement");
    assert_eq!(
        patched["liquidity"], "cash",
        "an override survives a kind change"
    );
}

#[tokio::test]
async fn kind_change_redefaults_liquidity() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Card", "kind": "credit_card", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"kind": "savings"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["liquidity"], "cash");
    assert_eq!(patched["liquidity_overridden"], false);
}

#[tokio::test]
async fn opening_balance_recorded_as_manual_snapshot() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "JPY",
               "opening_balance": {"on": "2026-10-01", "minor_units": -12345}}),
    )
    .await;

    let expected = json!({
        "on": "2026-10-01",
        "balance": {"minor_units": -12345, "currency": "JPY"},
        "source": "manual",
    });
    assert_eq!(created["latest_balance"], expected);
    let id = created["id"].as_str().unwrap();
    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances"),
        &write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page, json!({"items": [expected], "next": null}));
}

#[tokio::test]
async fn manual_balance_last_write_wins_and_lists_newest_first() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (_, read) = token(&world, Scope::Read).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    for (on, minor) in [
        ("2026-10-01", 100),
        ("2026-10-02", 200),
        ("2026-10-01", 150),
    ] {
        let (status, body) = call(
            &world.server,
            Method::PUT,
            &format!("/api/v1/accounts/{id}/balances/{on}"),
            &write,
            Some(json!({"minor_units": minor})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["balance"]["minor_units"], minor);
        assert_eq!(body["source"], "manual");
    }

    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?limit=1"),
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["on"], "2026-10-02");
    let next = page["next"].as_str().unwrap();
    let (_, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?limit=1&after={next}"),
        &read,
        None,
    )
    .await;
    assert_eq!(page["items"][0]["on"], "2026-10-01");
    assert_eq!(page["items"][0]["balance"]["minor_units"], 150);
    assert!(page["next"].is_null());

    let (status, body) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?after=SECRETVALUE"),
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");
    assert!(!body.to_string().contains("SECRET"), "{body}");

    let (status, body) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/2026-10-03"),
        &read,
        Some(json!({"minor_units": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this action needs write access to this account"
    );
}

#[tokio::test]
async fn closed_accounts_are_listed_only_on_request() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (_, read) = token(&world, Scope::Read).await;
    let kept = create(
        &world.server,
        &write,
        json!({"name": "Kept", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let closed = create(
        &world.server,
        &write,
        json!({"name": "Old", "kind": "savings", "currency": "USD"}),
    )
    .await;
    let closed_id = closed["id"].as_str().unwrap();
    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{closed_id}"),
        &write,
        Some(json!({"closed": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["closed"], true);

    let names = |body: &Value| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let (status, listed) = call(&world.server, Method::GET, "/api/v1/accounts", &read, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&listed), [kept["name"].as_str().unwrap()]);
    let (_, listed) = call(
        &world.server,
        Method::GET,
        "/api/v1/accounts?include_closed=true",
        &read,
        None,
    )
    .await;
    assert_eq!(names(&listed), ["Kept", "Old"]);
}

#[tokio::test]
async fn currency_is_immutable() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"currency": "EUR"})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid");
    let (_, fetched) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}"),
        &write,
        None,
    )
    .await;
    assert_eq!(fetched["currency"], "USD");
}

#[tokio::test]
async fn invalid_values_are_refused_without_echo() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    for body in [
        json!({"name": "", "kind": "checking", "currency": "USD"}),
        json!({"name": "SECRET\u{7}", "kind": "checking", "currency": "USD"}),
    ] {
        let (status, problem) = call(
            &world.server,
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["code"], "invalid");
        assert!(!problem.to_string().contains("SECRET"), "{problem}");
    }
    for body in [
        json!({"name": "A", "kind": "SECRETKIND", "currency": "USD"}),
        json!({"name": "A", "kind": "checking", "currency": "SECRET"}),
        json!({"name": "A", "kind": "checking", "currency": "USD", "SECRET": 1}),
    ] {
        let (status, problem) = call(
            &world.server,
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
        assert!(!problem.to_string().contains("SECRET"), "{problem}");
    }
    assert_eq!(account_count(&world.server).await, 0);
}

#[tokio::test]
async fn path_rejection_does_not_echo() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    for uri in [
        "/api/v1/accounts/SECRETVALUE",
        "/api/v1/accounts/SECRETVALUE/balances",
    ] {
        let (status, body) = call(&world.server, Method::GET, uri, &write, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["code"], "invalid");
        assert!(!body.to_string().contains("SECRET"), "{body}");
    }
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, body) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/SECRETDATE"),
        &write,
        Some(json!({"minor_units": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "the `on` parameter is not valid");
    assert!(!body.to_string().contains("SECRET"), "{body}");
}

#[tokio::test]
async fn unknown_account_is_not_found() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let missing = coffer_domain::ids::AccountId::new();
    for (method, uri, body) in [
        (Method::GET, format!("/api/v1/accounts/{missing}"), None),
        (
            Method::PATCH,
            format!("/api/v1/accounts/{missing}"),
            Some(json!({"name": "X"})),
        ),
        (
            Method::GET,
            format!("/api/v1/accounts/{missing}/balances"),
            None,
        ),
        (
            Method::PUT,
            format!("/api/v1/accounts/{missing}/balances/2026-10-01"),
            Some(json!({"minor_units": 1})),
        ),
    ] {
        let (status, problem) = call(&world.server, method.clone(), &uri, &write, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(problem["code"], "not_found");
    }
}

#[tokio::test]
async fn read_token_cannot_create_account() {
    let world = world().await;
    let (_, read) = token(&world, Scope::Read).await;

    let (status, body) = call(
        &world.server,
        Method::POST,
        "/api/v1/accounts",
        &read,
        Some(json!({"name": "Checking", "kind": "checking", "currency": "USD"})),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(account_count(&world.server).await, 0);

    let (status, body) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &read,
            Some(r#"{"name": "Checking", "kind": "checking", "currency": "USD"}"#),
            Some("read-key"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(account_count(&world.server).await, 0);
}

#[tokio::test]
async fn account_changes_audited_with_agent() {
    let world = world().await;
    let (agent, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD",
               "opening_balance": {"on": "2026-10-01", "minor_units": 500}}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"name": "Joint checking", "include_in_net_worth": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/2026-10-01"),
        &write,
        Some(json!({"minor_units": 750})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let by_agent = AuditActor::Member {
        member: world.alice.member,
        agent: Some(agent),
    };
    let created_events = audit_actions(&world.server, "account.create").await;
    assert_eq!(created_events.len(), 1);
    let event = &created_events[0];
    assert_eq!(event.actor, by_agent);
    assert_eq!(event.entity_id, id);
    let fields: Vec<_> = event.changes.iter().map(|(field, _)| field).collect();
    for field in [
        "name",
        "kind",
        "currency",
        "liquidity",
        "opening_balance_on",
    ] {
        assert!(fields.contains(&field), "{field} in {fields:?}");
    }

    let updates = audit_actions(&world.server, "account.update").await;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].actor, by_agent);
    let changes: Vec<_> = updates[0].changes.iter().collect();
    assert_eq!(changes.len(), 2, "only the changed fields: {changes:?}");
    let name = changes
        .iter()
        .find(|(field, _)| *field == "name")
        .unwrap()
        .1;
    assert_eq!(name.before, AuditValue::from("Checking"));
    assert_eq!(name.after, AuditValue::from("Joint checking"));

    let balances = audit_actions(&world.server, "balance.set").await;
    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0].actor, by_agent);
    let minor = balances[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "balance_minor")
        .unwrap()
        .1;
    assert_eq!(minor.before, AuditValue::Int(500));
    assert_eq!(minor.after, AuditValue::Int(750));

    // A patch that changes nothing records nothing.
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"name": "Joint checking"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        audit_actions(&world.server, "account.update").await.len(),
        1
    );
}

const NEW_ACCOUNT: &str = r#"{"name": "Savings", "kind": "savings", "currency": "USD"}"#;

#[tokio::test]
async fn idempotent_retry_replays_response() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let post = || {
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("retry-1"),
        )
    };

    let (first_status, first) = send(&world.server, post()).await;
    let (second_status, second) = send(&world.server, post()).await;

    assert_eq!(first_status, StatusCode::CREATED, "{first}");
    assert_eq!(second_status, StatusCode::CREATED, "{second}");
    assert_eq!(first["id"], second["id"]);
    assert_eq!(first, second);
    assert_eq!(account_count(&world.server).await, 1);
    assert_eq!(
        audit_actions(&world.server, "account.create").await.len(),
        1
    );

    // Without a key, every request creates.
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(account_count(&world.server).await, 2);
}

#[tokio::test]
async fn idempotency_key_reuse_with_other_body_refused() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("reuse-1"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(r#"{"name": "Other", "kind": "savings", "currency": "USD"}"#),
            Some("reuse-1"),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "idempotency_key_reuse");
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn failed_create_frees_its_idempotency_key() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let invalid = r#"{"name": "", "kind": "savings", "currency": "USD"}"#;
    for _ in 0..2 {
        let (status, body) = send(
            &world.server,
            request(
                Method::POST,
                "/api/v1/accounts",
                &write,
                Some(invalid),
                Some("fails-1"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["code"], "invalid");
    }
    let (status, body) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("fails-1"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn idempotency_key_header_is_validated() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let too_long = "k".repeat(129);
    for key in ["", "has space", "caf\u{e9}", too_long.as_str()] {
        let mut builder = Request::post("/api/v1/accounts")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", write.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json");
        builder = builder.header(
            "idempotency-key",
            axum::http::HeaderValue::from_bytes(key.as_bytes()).unwrap(),
        );
        let (status, body) = send(
            &world.server,
            builder.body(Body::from(NEW_ACCOUNT)).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?}");
        assert_eq!(body["code"], "invalid");
        assert_eq!(body["detail"], "the `Idempotency-Key` header is not valid");
    }
    let longest = "k".repeat(128);
    let (status, _) = send(
        &world.server,
        request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some(&longest),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn concurrent_same_key_creates_once() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let post = || {
        router(world.server.app_state()).oneshot(request(
            Method::POST,
            "/api/v1/accounts",
            &write,
            Some(NEW_ACCOUNT),
            Some("concurrent-1"),
        ))
    };

    let (a, b) = tokio::join!(post(), post());

    let mut results = Vec::new();
    for response in [a.unwrap(), b.unwrap()] {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        results.push((status, body));
    }
    results.sort_by_key(|(status, _)| *status);
    let (first_status, first) = &results[0];
    assert_eq!(*first_status, StatusCode::CREATED, "{first}");
    let (second_status, second) = &results[1];
    match *second_status {
        StatusCode::CREATED => assert_eq!(second["id"], first["id"]),
        StatusCode::CONFLICT => assert_eq!(second["code"], "idempotency_in_progress"),
        other => panic!("unexpected {other}: {second}"),
    }
    assert_eq!(account_count(&world.server).await, 1);
}

#[tokio::test]
async fn oversize_body_is_invalid() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let padding = "x".repeat(256 * 1024);
    let body = format!(r#"{{"name": "{padding}", "kind": "savings", "currency": "USD"}}"#);

    let (status, problem) = send(
        &world.server,
        request(Method::POST, "/api/v1/accounts", &write, Some(&body), None),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["code"], "invalid");
    assert_eq!(
        problem["detail"],
        "the request body is not valid JSON for this operation"
    );
    assert_eq!(account_count(&world.server).await, 0);
}
