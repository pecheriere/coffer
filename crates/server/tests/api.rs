//! The JSON API and health endpoint, through the real router.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::body::Body;
use axum::extract::FromRequest as _;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse as _;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::MemberId;
use coffer_server::assets::CSS_BASE_CSS;
use coffer_server::http::api::extract::{TypedJson, TypedPath};
use coffer_server::http::problem::{Problem, ProblemCode};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
    bob: MemberId,
}

/// Admin "alice" and member "bob".
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let alice_id = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member: alice_id,
        role: Role::Admin,
        via: Via::Session,
    };
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let bob = server.authenticator().enroll(&invite, "bob", "Bob").await;
    World { server, alice, bob }
}

async fn token(world: &World, scope: Scope) -> SecretToken {
    world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            scope,
            None,
        )
        .await
        .unwrap()
        .1
}

async fn get(server: &TestServer, uri: &str, token: Option<&SecretToken>) -> (StatusCode, Value) {
    let mut request = Request::get(uri);
    if let Some(token) = token {
        request = request.header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
    }
    let response = router(server.app_state())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[tokio::test]
async fn me_identifies_agent_and_member() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    let (status, body) = get(&world.server, "/api/v1/me", Some(&token)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["member"]["username"], "alice");
    assert_eq!(body["agent"]["scope"], "read");
}

#[tokio::test]
async fn members_lists_everyone_with_disabled_state() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;
    world
        .server
        .identity
        .disable_member(&world.alice, world.bob)
        .await
        .unwrap();

    let (status, body) = get(&world.server, "/api/v1/members", Some(&token)).await;

    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let bob = items.iter().find(|m| m["username"] == "bob").unwrap();
    assert_eq!(bob["display_name"], "Bob");
    assert_eq!(bob["role"], "member");
    assert!(bob["disabled_at"].is_string());
    let alice = items.iter().find(|m| m["username"] == "alice").unwrap();
    assert!(alice["disabled_at"].is_null());
    assert!(items.iter().all(|m| m.get("credentials").is_none()));
}

#[tokio::test]
async fn api_requires_a_bearer_token() {
    let world = world().await;
    for uri in ["/api/v1/members", "/api/v1/audit"] {
        let (status, body) = get(&world.server, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body["code"], "unauthenticated");
    }
}

#[tokio::test]
async fn audit_shows_agent_attribution() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    let (status, body) = get(
        &world.server,
        "/api/v1/audit?entity_type=member",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created: Vec<&Value> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "member.create")
        .collect();
    assert_eq!(created.len(), 2);
    // Each member created themselves by redeeming a link.
    for event in &created {
        assert_eq!(event["actor"]["kind"], "member");
        assert_eq!(event["actor"]["member"], event["entity_id"]);
        assert!(event["actor"]["agent"].is_null());
        assert!(event["at"].as_str().unwrap().ends_with('Z'));
    }

    let (_, body) = get(
        &world.server,
        "/api/v1/audit?entity_type=agent_token",
        Some(&token),
    )
    .await;
    let items = body["items"].as_array().unwrap();
    let event = items
        .iter()
        .find(|e| e["action"] == "token.create")
        .unwrap();
    assert_eq!(event["actor"]["kind"], "member");
    assert_eq!(event["actor"]["member"], world.alice.member.to_string());
    assert!(event["actor"]["agent"].is_null());
    // Changes are `{field: {before, after}}`.
    assert_eq!(
        event["changes"]["scope"],
        serde_json::json!({ "before": null, "after": "read" })
    );

    // Filtering by the member returns only that member's actions.
    let uri = format!("/api/v1/audit?member={}", world.bob);
    let (_, body) = get(&world.server, &uri, Some(&token)).await;
    let items = body["items"].as_array().unwrap();
    assert_ne!(items.len(), 0);
    assert!(
        items
            .iter()
            .all(|e| e["actor"]["member"] == world.bob.to_string())
    );
}

#[tokio::test]
async fn audit_pagination_via_api() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    let (_, first) = get(&world.server, "/api/v1/audit?limit=2", Some(&token)).await;
    let first_items = first["items"].as_array().unwrap();
    assert_eq!(first_items.len(), 2);
    let cursor = first["next"].as_str().unwrap().to_owned();

    let (status, second) = get(
        &world.server,
        &format!("/api/v1/audit?limit=2&after={cursor}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second_items = second["items"].as_array().unwrap();
    assert_ne!(second_items.len(), 0);
    assert!(second_items.iter().all(|e| !first_items.contains(e)));

    let (_, all) = get(&world.server, "/api/v1/audit?limit=200", Some(&token)).await;
    assert!(all["next"].is_null());
}

#[tokio::test]
async fn audit_rejects_bad_cursor_and_bad_params_without_echo() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    let (status, body) = get(
        &world.server,
        "/api/v1/audit?after=not-a-cursor",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");

    for uri in [
        "/api/v1/audit?from=yesterdaySECRET",
        "/api/v1/audit?to=2026-13-45",
        "/api/v1/audit?member=SECRETid",
        "/api/v1/audit?agent=SECRETid",
        "/api/v1/audit?limit=SECRET",
        "/api/v1/audit?SECRETparam=1",
        "/api/v1/audit?limit=1&limit=2",
        "/api/v1/audit?entity_type=SECRET&entity_type=other",
    ] {
        let (status, body) = get(&world.server, uri, Some(&token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["code"], "invalid");
        assert!(!body.to_string().contains("SECRET"), "{uri}");
    }
}

#[tokio::test]
async fn audit_filters_by_time_range() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    let (_, body) = get(
        &world.server,
        "/api/v1/audit?from=2100-01-01T00:00:00Z",
        Some(&token),
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    let (_, body) = get(
        &world.server,
        "/api/v1/audit?to=2000-01-01T00:00:00Z",
        Some(&token),
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

async fn send(
    server: &TestServer,
    method: &str,
    uri: &str,
    token: Option<&SecretToken>,
) -> axum::http::Response<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
    }
    router(server.app_state())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn problem_code(response: axum::http::Response<Body>) -> String {
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["code"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn unknown_api_route_problem_json() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    for token in [Some(&token), None] {
        for uri in [
            "/api",
            "/api/",
            "/api/v1",
            "/api/v1/",
            "/api/v1/nope/deeper",
            "/api/v2/me",
        ] {
            let response = send(&world.server, "GET", uri, token).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(problem_code(response).await, "not_found", "{uri}");
        }
    }
}

#[tokio::test]
async fn wrong_method_under_api_is_405_problem_json() {
    let world = world().await;
    let token = token(&world, Scope::Admin).await;

    for (method, uri) in [
        ("POST", "/api/v1/me"),
        ("DELETE", "/api/v1/members"),
        ("PUT", "/api/v1/audit"),
    ] {
        let response = send(&world.server, method, uri, Some(&token)).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
        assert!(response.headers().contains_key(header::ALLOW));
        assert_eq!(problem_code(response).await, "method_not_allowed");
    }
}

/// Every response carries the security headers, including the problems
/// `problem_bodies` writes for unmatched `/api` paths and methods.
#[tokio::test]
async fn security_headers_on_every_response() {
    let world = world().await;
    let token = token(&world, Scope::Admin).await;

    for (method, uri, status) in [
        ("GET", "/api/nope", StatusCode::NOT_FOUND),
        ("DELETE", "/api/v1/me", StatusCode::METHOD_NOT_ALLOWED),
        ("GET", "/api/v1/me", StatusCode::OK),
        ("GET", "/login", StatusCode::OK),
        ("GET", "/nope", StatusCode::NOT_FOUND),
        ("GET", CSS_BASE_CSS.url, StatusCode::OK),
        (
            "GET",
            "/static/css/base.0000000000000000.css",
            StatusCode::NOT_FOUND,
        ),
        ("GET", "/healthz", StatusCode::OK),
    ] {
        let response = send(&world.server, method, uri, Some(&token)).await;
        assert_eq!(response.status(), status, "{method} {uri}");
        let headers = response.headers();
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
             form-action 'self'; base-uri 'none'",
            "{method} {uri}"
        );
        assert_eq!(
            headers[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "{method} {uri}"
        );
        assert_eq!(
            headers[header::REFERRER_POLICY],
            "same-origin",
            "{method} {uri}"
        );
        if status == StatusCode::METHOD_NOT_ALLOWED {
            assert_eq!(headers[header::ALLOW], "GET,HEAD", "{method} {uri}");
        }
    }
}

#[tokio::test]
async fn disabled_members_token_is_401() {
    let world = world().await;
    let bob = Actor {
        member: world.bob,
        role: Role::Member,
        via: Via::Session,
    };
    let (_, bob_token) = world
        .server
        .agent_tokens
        .create(&bob, TokenName::parse("bobs").unwrap(), Scope::Read, None)
        .await
        .unwrap();
    let (status, _) = get(&world.server, "/api/v1/members", Some(&bob_token)).await;
    assert_eq!(status, StatusCode::OK);

    world
        .server
        .identity
        .disable_member(&world.alice, world.bob)
        .await
        .unwrap();

    let (status, body) = get(&world.server, "/api/v1/members", Some(&bob_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn healthz_ok() {
    let world = world().await;

    let response = router(world.server.app_state())
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"ok");
}

#[tokio::test]
async fn problem_type_is_urn_of_code() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    for (uri, token) in [
        ("/api/v1/members", None),
        ("/api/v1/nope", Some(&token)),
        ("/api/v1/audit?limit=x", Some(&token)),
        ("/api/v1/audit?after=not-a-cursor", Some(&token)),
    ] {
        let (_, body) = get(&world.server, uri, token).await;
        let code = body["code"].as_str().unwrap();
        assert_eq!(body["type"], format!("urn:coffer:problem:{code}"), "{uri}");
    }
}

#[tokio::test]
async fn query_rejection_does_not_echo() {
    let world = world().await;
    let token = token(&world, Scope::Read).await;

    for (uri, detail) in [
        (
            "/api/v1/audit?limit=SECRETVALUE",
            "the `limit` parameter is not valid",
        ),
        (
            "/api/v1/audit?SECRETNAME=1",
            "a query parameter is not recognised or is repeated",
        ),
        (
            "/api/v1/audit?member=SECRETVALUE",
            "the `member` parameter is not valid",
        ),
        (
            "/api/v1/audit?member=SECRETVALUE&member=SECRETVALUE",
            "a query parameter is not recognised or is repeated",
        ),
    ] {
        let (status, body) = get(&world.server, uri, Some(&token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["code"], "invalid", "{uri}");
        assert_eq!(body["detail"], detail, "{uri}");
        assert!(!body.to_string().contains("SECRET"), "{uri}");
    }
}

/// The body of a rejection, checked to be a fixed `400 invalid` problem.
async fn rejection_body(problem: Problem) -> Value {
    let response = problem.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "invalid");
    assert!(!body.to_string().contains("SECRET"), "{body}");
    body
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct OneId {
    /// The thing.
    id: MemberId,
}

#[tokio::test]
async fn path_rejection_does_not_echo() {
    let app = axum::Router::new().route(
        "/things/{id}",
        axum::routing::get(|TypedPath(path): TypedPath<OneId>| async move { path.id.to_string() }),
    );

    let ok = MemberId::new();
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/things/{ok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::get("/things/SECRETVALUE")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "invalid");
    assert_eq!(body["detail"], "the `id` parameter is not valid");
    assert!(!body.to_string().contains("SECRET"), "{body}");
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Plain,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Body1 {
    count: u32,
    kind: Kind,
}

#[tokio::test]
async fn json_rejection_does_not_echo() {
    for body in SECRET_BODIES {
        let request = Request::post("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let problem = TypedJson::<Body1>::from_request(request, &())
            .await
            .unwrap_err();
        let body = rejection_body(problem).await;
        assert_eq!(
            body["detail"],
            "the request body is not valid JSON for this operation"
        );
    }

    let request = Request::post("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"count": 1, "kind": "plain"}"#))
        .unwrap();
    let TypedJson(parsed) = TypedJson::<Body1>::from_request(request, &())
        .await
        .unwrap();
    assert_eq!(parsed.count, 1);
    assert!(matches!(parsed.kind, Kind::Plain));
}

/// Bodies whose rejection must not put `SECRET` in a log: as a value, an
/// unknown field name, an unknown enum variant and inside broken JSON.
const SECRET_BODIES: [&str; 5] = [
    r#"{"count": 1, "kind": "plain", "SECRETNAME": 1}"#,
    r#"{"count": "SECRETVALUE", "kind": "plain"}"#,
    r#"{"count": 1, "kind": "SECRETVALUE"}"#,
    r#"{"SECRETVALUE"#,
    r#"{"count": 1, "kind": {"SECRETVALUE": 1}}"#,
];

#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn rejected_body_logs_no_input() {
    use tracing::instrument::WithSubscriber as _;

    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    async {
        tracing::debug!("capture-check-KNOWN-MARKER");
        for body in SECRET_BODIES {
            let request = Request::post("/")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            assert!(
                TypedJson::<Body1>::from_request(request, &())
                    .await
                    .is_err()
            );
        }
    }
    .with_subscriber(subscriber)
    .await;

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("capture-check-KNOWN-MARKER"), "{text}");
    assert_eq!(
        text.matches("request body rejected").count(),
        SECRET_BODIES.len(),
        "{text}"
    );
    assert!(
        text.contains("path=count"),
        "a documented field is named: {text}"
    );
    assert!(text.contains("category=data"), "{text}");
    assert!(text.contains("category=eof"), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
}

#[tokio::test]
async fn retry_after_header_emitted() {
    let response = Problem::new(StatusCode::TOO_MANY_REQUESTS, ProblemCode::Busy, "Busy")
        .with_retry_after(30)
        .into_response();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "30");

    let response =
        Problem::new(StatusCode::TOO_MANY_REQUESTS, ProblemCode::Busy, "Busy").into_response();
    assert!(!response.headers().contains_key(header::RETRY_AFTER));
}
