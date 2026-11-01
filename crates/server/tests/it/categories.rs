//! Category groups and categories through the real router with agent
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
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
}

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

async fn call(
    server: &TestServer,
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri).header(
        header::AUTHORIZATION,
        format!("Bearer {}", token.expose_secret()),
    );
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
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

async fn audit(server: &TestServer, entity_type: &str) -> Vec<AuditEvent> {
    server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some(entity_type.into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items
}

/// Sends a request that must answer `expected`; returns the body.
async fn expect(
    server: &TestServer,
    token: &SecretToken,
    (method, uri): (Method, &str),
    body: Option<Value>,
    key: Option<&str>,
    expected: StatusCode,
) -> Value {
    let (status, answer) = call(server, method, uri, token, body, key).await;
    assert_eq!(status, expected);
    answer
}

/// Each group's name with its categories' names, in listing order.
async fn tree_names(server: &TestServer, token: &SecretToken) -> Vec<(String, Vec<String>)> {
    let get = (Method::GET, "/api/v1/categories");
    let tree = expect(server, token, get, None, None, StatusCode::OK).await;
    let name = |value: &Value| value["name"].as_str().unwrap().to_owned();
    tree["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            let categories = group["categories"].as_array().unwrap();
            (name(group), categories.iter().map(name).collect())
        })
        .collect()
}

/// Every category change in `category_crud_via_api_audited` was made by
/// alice's agent, and the replayed create recorded nothing.
async fn check_audit(server: &TestServer, member: MemberId, agent: AgentTokenId, food_id: &str) {
    let by_agent = AuditActor::Member {
        member,
        agent: Some(agent),
    };
    let group_events = audit(server, "category_group").await;
    let mut actions: Vec<_> = group_events.iter().map(|e| e.action.as_str()).collect();
    actions.sort_unstable();
    assert_eq!(
        actions,
        [
            "category_group.create",
            "category_group.create",
            "category_group.update"
        ],
        "the replay created nothing"
    );
    assert!(group_events.iter().all(|e| e.actor == by_agent));
    let category_events = audit(server, "category").await;
    assert_eq!(category_events.len(), 3);
    assert!(category_events.iter().all(|e| e.actor == by_agent));
    let update = category_events
        .iter()
        .find(|e| e.action == "category.update")
        .unwrap();
    assert_eq!(update.entity_id, food_id);
    let fields: Vec<_> = update.changes.iter().map(|(field, _)| field).collect();
    assert_eq!(
        fields,
        ["archived", "group", "name"],
        "only the changed fields"
    );
    let name = update.changes.iter().find(|(f, _)| *f == "name").unwrap().1;
    assert_eq!(name.before, AuditValue::from("Food"));
    assert_eq!(name.after, AuditValue::from("Groceries"));
}

#[tokio::test]
async fn category_crud_via_api_audited() {
    let world = world().await;
    let (agent, write) = token(&world, Scope::Write).await;
    let server = &world.server;
    let groups = (Method::POST, "/api/v1/category-groups");
    let categories = (Method::POST, "/api/v1/categories");
    let created = StatusCode::CREATED;

    assert_eq!(tree_names(server, &write).await, []);

    let body = json!({"name": "Living"});
    let living = expect(
        server,
        &write,
        groups.clone(),
        Some(body.clone()),
        Some("g1"),
        created,
    )
    .await;
    let shape = json!({"id": living["id"], "name": "Living", "position": 0, "archived": false,
                       "categories": []});
    assert_eq!(living, shape);
    let living_id = living["id"].as_str().unwrap();
    // The same create retried replays the first response.
    let replay = expect(
        server,
        &write,
        groups.clone(),
        Some(body),
        Some("g1"),
        created,
    )
    .await;
    assert_eq!(replay, living);

    let body = json!({"name": "Fun", "position": 0});
    let fun = expect(server, &write, groups, Some(body), None, created).await;
    let fun_id = fun["id"].as_str().unwrap();

    let body = json!({"group": living_id, "name": "Food", "kind": "expense"});
    let food = expect(
        server,
        &write,
        categories.clone(),
        Some(body),
        Some("c1"),
        created,
    )
    .await;
    assert_eq!(food["group"], living_id);
    assert_eq!(food["kind"], "expense");
    assert_eq!(food["archived"], false);
    let food_id = food["id"].as_str().unwrap();
    let body = json!({"group": living_id, "name": "Salary", "kind": "income"});
    expect(server, &write, categories, Some(body), None, created).await;

    let food_uri = format!("/api/v1/categories/{food_id}");
    let body = json!({"name": "Groceries", "group": fun_id, "archived": true});
    let patch_food = (Method::PATCH, food_uri.as_str());
    let patched = expect(
        server,
        &write,
        patch_food.clone(),
        Some(body),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(patched["name"], "Groceries");
    assert_eq!(patched["group"], fun_id);
    assert_eq!(patched["archived"], true);
    let living_uri = format!("/api/v1/category-groups/{living_id}");
    let body = json!({"name": "Household", "position": 7});
    let patch_living = (Method::PATCH, living_uri.as_str());
    let group = expect(
        server,
        &write,
        patch_living,
        Some(body),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(group["name"], "Household");
    assert_eq!(group["categories"][0]["name"], "Salary");

    let names = |group: &str, categories: &[&str]| {
        (
            group.to_owned(),
            categories
                .iter()
                .map(|c| (*c).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        tree_names(server, &write).await,
        [
            names("Fun", &["Groceries"]),
            names("Household", &["Salary"])
        ]
    );

    check_audit(server, world.alice.member, agent, food_id).await;

    // A patch that changes nothing records nothing.
    let body = json!({"name": "Groceries", "archived": true});
    expect(server, &write, patch_food, Some(body), None, StatusCode::OK).await;
    assert_eq!(audit(server, "category").await.len(), 3);
}

#[tokio::test]
async fn category_requests_are_validated_without_echo() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (_, read) = token(&world, Scope::Read).await;
    let server = &world.server;

    let (status, _) = call(
        server,
        Method::POST,
        "/api/v1/category-groups",
        &read,
        Some(json!({"name": "Living"})),
        Some("read-key"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(server, Method::GET, "/api/v1/categories", &read, None, None).await;
    assert_eq!(status, StatusCode::OK, "a read token lists categories");

    let (_, group) = call(
        server,
        Method::POST,
        "/api/v1/category-groups",
        &write,
        Some(json!({"name": "Living"})),
        None,
    )
    .await;
    let group_id = group["id"].as_str().unwrap();
    for (uri, body) in [
        ("/api/v1/category-groups", json!({"name": "SECRET\u{7}"})),
        (
            "/api/v1/category-groups",
            json!({"name": "x", "position": 10_001}),
        ),
        (
            "/api/v1/categories",
            json!({"group": group_id, "name": "Food", "kind": "SECRETKIND"}),
        ),
        (
            "/api/v1/categories",
            json!({"group": coffer_domain::ids::CategoryGroupId::new(), "name": "SECRET",
                   "kind": "expense"}),
        ),
        (
            "/api/v1/categories",
            json!({"group": group_id, "name": "Food", "kind": "expense", "SECRETFIELD": 1}),
        ),
    ] {
        let (status, problem) = call(server, Method::POST, uri, &write, Some(body), None).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{status}"
        );
        assert!(
            !problem.to_string().contains("SECRET"),
            "the response must not contain the secret"
        );
    }
    let (status, _) = call(
        server,
        Method::PATCH,
        &format!(
            "/api/v1/categories/{}",
            coffer_domain::ids::CategoryId::new()
        ),
        &write,
        Some(json!({"name": "Food"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        server,
        Method::PATCH,
        "/api/v1/category-groups/SECRET",
        &write,
        Some(json!({"name": "Food"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
