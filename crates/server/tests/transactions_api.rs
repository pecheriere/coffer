//! The transactions API through the real router with agent tokens: listing
//! with filters and cursors, single changes, and atomic review batches.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, DisplayName, Role, Scope, TokenName, Username, Via};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::time::Clock as _;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::{AuditEvent, AuditFilter, NewMember, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

struct World {
    server: TestServer,
    alice: Actor,
    write_id: AgentTokenId,
    write: SecretToken,
}

/// Admin "alice", signed in with a passkey, and a write token of hers.
async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (write_id, write) = token_for(&server, &alice, Scope::Write).await;
    World {
        server,
        alice,
        write_id,
        write,
    }
}

async fn token_for(
    server: &TestServer,
    alice: &Actor,
    scope: Scope,
) -> (AgentTokenId, SecretToken) {
    let (record, secret) = server
        .agent_tokens
        .create(alice, TokenName::parse("claude").unwrap(), scope, None)
        .await
        .unwrap();
    (record.id, secret)
}

async fn read_token(world: &World) -> SecretToken {
    token_for(&world.server, &world.alice, Scope::Read).await.1
}

async fn member(world: &World, username: &str) -> MemberId {
    let mut tx = world.server.store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse(username).unwrap(),
            display_name: DisplayName::parse(username).unwrap(),
            role: Role::Member,
            created_at: world.server.clock.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    member.id
}

fn request(
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<&Value>,
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
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

/// The status and the raw body text.
async fn send_raw(server: &TestServer, request: Request<Body>) -> (StatusCode, String) {
    let response = router(server.app_state()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn call(
    server: &TestServer,
    method: Method,
    uri: &str,
    token: &SecretToken,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, text) = send_raw(server, request(method, uri, token, body.as_ref(), None)).await;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

async fn account(world: &World, name: &str) -> String {
    let (status, created) = call(
        &world.server,
        Method::POST,
        "/api/v1/accounts",
        &world.write,
        Some(json!({"name": name, "kind": "checking", "currency": "USD"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    id(&created)
}

async fn category(world: &World, name: &str) -> String {
    let (status, group) = call(
        &world.server,
        Method::POST,
        "/api/v1/category-groups",
        &world.write,
        Some(json!({"name": format!("{name} group")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, created) = call(
        &world.server,
        Method::POST,
        "/api/v1/categories",
        &world.write,
        Some(json!({"group": id(&group), "name": name, "kind": "expense"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    id(&created)
}

async fn transaction(
    world: &World,
    account: &str,
    posted_on: &str,
    minor_units: i64,
    description: &str,
) -> String {
    let (status, created) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions",
        &world.write,
        Some(json!({"account": account, "posted_on": posted_on,
                    "minor_units": minor_units, "description": description})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    id(&created)
}

async fn patch(world: &World, transaction: &str, body: Value) -> Value {
    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/transactions/{transaction}"),
        &world.write,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    patched
}

async fn get(world: &World, transaction: &str) -> Value {
    let (status, fetched) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/transactions/{transaction}"),
        &world.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    fetched
}

/// One page of `GET /api/v1/transactions?<query>`.
async fn list_page(world: &World, query: &str) -> Value {
    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/transactions?{query}"),
        &world.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{query}");
    page
}

/// The ids of the first page, in order.
async fn listed(world: &World, query: &str) -> Vec<String> {
    let page = list_page(world, query).await;
    page["items"].as_array().unwrap().iter().map(id).collect()
}

async fn transaction_updates(world: &World) -> Vec<AuditEvent> {
    world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("transaction".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|event| event.action == "transaction.update")
        .collect()
}

fn ids(expected: &[&String]) -> Vec<String> {
    expected.iter().map(|id| (*id).clone()).collect()
}

#[tokio::test]
async fn filters_combine() {
    let world = world().await;
    let bob = member(&world, "bob").await.to_string();
    let a = account(&world, "Checking").await;
    let b = account(&world, "Savings").await;
    let c1 = category(&world, "Groceries").await;
    let c2 = category(&world, "Transport").await;
    let c3 = category(&world, "Dining").await;

    let t1 = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let t2 = transaction(&world, &a, "2026-10-02", -2000, "Bus pass").await;
    let t3 = transaction(&world, &b, "2026-10-03", -3000, "Dinner").await;
    let t4 = transaction(&world, &a, "2026-10-04", 5000, "Refund").await;
    let t5 = transaction(&world, &b, "2026-10-05", -500, "Snack").await;
    patch(
        &world,
        &t1,
        json!({"splits": [{"category": c1, "minor_units": -1000}], "assignee": bob}),
    )
    .await;
    patch(
        &world,
        &t2,
        json!({"splits": [{"category": c2, "minor_units": -2000}], "review": "reviewed"}),
    )
    .await;
    patch(
        &world,
        &t3,
        json!({"splits": [{"category": c3, "minor_units": -3000}], "assignee": bob,
               "payee": "City Grocer"}),
    )
    .await;
    patch(
        &world,
        &t4,
        json!({"splits": [{"category": c1, "minor_units": 5000}]}),
    )
    .await;
    patch(
        &world,
        &t5,
        json!({"splits": [{"category": c2, "minor_units": -500}], "review": "reviewed",
               "assignee": bob, "notes": "bought at the GROCER downstairs"}),
    )
    .await;

    let cases: Vec<(String, Vec<String>)> = vec![
        (String::new(), ids(&[&t5, &t4, &t3, &t2, &t1])),
        (format!("account={a}"), ids(&[&t4, &t2, &t1])),
        (format!("account={a},{b}"), ids(&[&t5, &t4, &t3, &t2, &t1])),
        ("from=2026-10-02&to=2026-10-04".into(), ids(&[&t3, &t2])),
        ("review=reviewed".into(), ids(&[&t5, &t2])),
        ("review=unreviewed".into(), ids(&[&t4, &t3, &t1])),
        (format!("assignee={bob}"), ids(&[&t5, &t3, &t1])),
        ("assignee=none".into(), ids(&[&t4, &t2])),
        (format!("category={c1}"), ids(&[&t4, &t1])),
        (format!("category={c1},{c3}"), ids(&[&t4, &t3, &t1])),
        ("pending=false".into(), ids(&[&t5, &t4, &t3, &t2, &t1])),
        ("pending=true".into(), Vec::new()),
        ("flag=possible_duplicate".into(), Vec::new()),
        (
            "min_amount=-2000&max_amount=-500".into(),
            ids(&[&t5, &t2, &t1]),
        ),
        ("min_amount=0".into(), ids(&[&t4])),
        ("q=grocer".into(), ids(&[&t5, &t3, &t1])),
        (
            format!("review=unreviewed&assignee={bob}&account={a}"),
            ids(&[&t1]),
        ),
    ];
    for (query, expected) in cases {
        assert_eq!(listed(&world, &query).await, expected, "query `{query}`");
    }
}

#[tokio::test]
async fn unknown_account_filter_is_404() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let missing = coffer_domain::ids::AccountId::new();
    let (status, body) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/transactions?account={a},{missing}"),
        &world.write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn uncategorized_filter() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let c1 = category(&world, "Groceries").await;
    let categorized = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let open = transaction(&world, &a, "2026-10-02", -700, "Unknown shop").await;
    patch(
        &world,
        &categorized,
        json!({"splits": [{"category": c1, "minor_units": -1000}]}),
    )
    .await;

    assert_eq!(
        listed(&world, "uncategorized=true").await,
        vec![open.clone()]
    );
    assert_eq!(
        listed(&world, "uncategorized=false").await,
        vec![open.clone(), categorized.clone()],
        "`false` does not filter"
    );

    patch(
        &world,
        &open,
        json!({"splits": [{"category": c1, "minor_units": -700}]}),
    )
    .await;
    assert_eq!(
        listed(&world, "uncategorized=true").await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn text_search_escapes_wildcards() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let percent = transaction(&world, &a, "2026-10-01", -1000, "50% off sale").await;
    transaction(&world, &a, "2026-10-02", -1000, "500 widgets").await;
    transaction(&world, &a, "2026-10-03", -1000, "Save 50 percent").await;
    let underscore = transaction(&world, &a, "2026-10-04", -1000, "Item 5_0").await;
    transaction(&world, &a, "2026-10-05", -1000, "Item 590").await;
    let backslash = transaction(&world, &a, "2026-10-06", -1000, r"Path a\b").await;

    assert_eq!(listed(&world, "q=50%25").await, vec![percent]);
    assert_eq!(listed(&world, "q=5_0").await, vec![underscore]);
    assert_eq!(listed(&world, "q=a%5Cb").await, vec![backslash]);
}

#[tokio::test]
async fn pagination_is_stable_across_equal_dates() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let mut created = Vec::new();
    for n in 0..5 {
        created.push(transaction(&world, &a, "2026-10-01", -100 - n, "Same day").await);
    }

    let mut seen = Vec::new();
    let mut query = "limit=2".to_owned();
    let mut pages = 0;
    loop {
        let page = list_page(&world, &query).await;
        pages += 1;
        let items: Vec<String> = page["items"].as_array().unwrap().iter().map(id).collect();
        assert!(items.len() <= 2);
        seen.extend(items);
        match page["next"].as_str() {
            Some(next) => query = format!("limit=2&after={next}"),
            None => break,
        }
        assert!(pages < 10, "pagination does not end");
    }
    assert_eq!(pages, 3);
    // Ids are UUIDv7, so newest first is the reverse of creation order.
    created.reverse();
    assert_eq!(
        seen, created,
        "every transaction exactly once, id descending"
    );
}

#[tokio::test]
async fn bad_cursor_is_400_invalid_cursor() {
    let world = world().await;
    for cursor in ["garbage", "2026-10-01~not-an-id", "2026-13-01~", "~"] {
        let (status, body) = call(
            &world.server,
            Method::GET,
            &format!("/api/v1/transactions?after={cursor}"),
            &world.write,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{cursor}");
        assert_eq!(body["code"], "invalid_cursor", "{cursor}");
    }
}

#[tokio::test]
async fn enum_query_value_not_echoed() {
    let world = world().await;
    let long = format!("SECRET{}", "x".repeat(100));
    for (query, parameter) in [
        ("review=SECRETVALUE".to_owned(), "review"),
        ("flag=SECRETVALUE".to_owned(), "flag"),
        ("assignee=SECRETVALUE".to_owned(), "assignee"),
        ("account=SECRETVALUE".to_owned(), "account"),
        ("category=SECRETVALUE".to_owned(), "category"),
        ("min_amount=SECRETVALUE".to_owned(), "min_amount"),
        (format!("q={long}"), "q"),
    ] {
        let (status, body) = send_raw(
            &world.server,
            request(
                Method::GET,
                &format!("/api/v1/transactions?{query}"),
                &world.write,
                None,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert!(!body.contains("SECRET"), "{query} was echoed");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["code"], "invalid");
        assert_eq!(
            body["detail"],
            format!("the `{parameter}` parameter is not valid")
        );
    }
}

#[tokio::test]
async fn batch_is_atomic() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let live = category(&world, "Groceries").await;
    let archived = category(&world, "Old").await;
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/categories/{archived}"),
        &world.write,
        Some(json!({"archived": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let second = transaction(&world, &a, "2026-10-02", -2000, "Old habit").await;
    let before = get(&world, &first).await;

    let (status, body) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions/batch",
        &world.write,
        Some(json!({"operations": [
            {"id": first, "patch": {"review": "reviewed",
                                    "splits": [{"category": live, "minor_units": -1000}]}},
            {"id": second, "patch": {"splits": [{"category": archived, "minor_units": -2000}]}},
        ]})),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid");
    let detail = body["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("operations[1]: "),
        "the detail must name the failing operation"
    );
    assert!(
        !detail.contains(&second) && !detail.contains(&archived),
        "the detail must not echo the ids"
    );
    assert_eq!(
        get(&world, &first).await,
        before,
        "the first operation rolled back"
    );
    assert_eq!(
        transaction_updates(&world).await.len(),
        0,
        "nothing audited"
    );
}

#[tokio::test]
async fn batch_names_a_missing_transaction_by_index() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let missing = coffer_domain::ids::TransactionId::new().to_string();

    let (status, body) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions/batch",
        &world.write,
        Some(json!({"operations": [
            {"id": first, "patch": {"review": "reviewed"}},
            {"id": missing, "patch": {"review": "reviewed"}},
        ]})),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "operations[1]: not found");
    assert_eq!(get(&world, &first).await["review"], "unreviewed");
}

#[tokio::test]
async fn batch_refuses_empty_oversized_and_repeated_operations() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let op = |id: &str| json!({"id": id, "patch": {"review": "reviewed"}});
    let oversized: Vec<Value> = (0..201)
        .map(|_| op(&coffer_domain::ids::TransactionId::new().to_string()))
        .collect();

    for operations in [Vec::new(), oversized, vec![op(&first), op(&first)]] {
        let (status, body) = call(
            &world.server,
            Method::POST,
            "/api/v1/transactions/batch",
            &world.write,
            Some(json!({ "operations": operations })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid");
    }
    assert_eq!(get(&world, &first).await["review"], "unreviewed");
}

#[tokio::test]
async fn batch_review_and_assign_attributed_to_agent() {
    let world = world().await;
    let bob = member(&world, "bob").await;
    let a = account(&world, "Checking").await;
    let groceries = category(&world, "Groceries").await;
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let second = transaction(&world, &a, "2026-10-02", -2000, "Market").await;

    let (status, body) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions/batch",
        &world.write,
        Some(json!({"operations": [
            {"id": second, "patch": {"review": "reviewed", "assignee": bob,
                                     "splits": [{"category": groceries, "minor_units": -2000}]}},
            {"id": first, "patch": {"review": "reviewed", "assignee": bob}},
        ]})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(
        items.iter().map(id).collect::<Vec<_>>(),
        vec![second.clone(), first.clone()],
        "in operation order"
    );
    let alice = world.alice.member.to_string();
    let agent = world.write_id.to_string();
    for item in items {
        assert_eq!(item["review"], "reviewed");
        assert_eq!(item["assignee"], bob.to_string());
        assert_eq!(item["reviewed_by"]["member"], alice);
        assert_eq!(item["reviewed_by"]["agent"], agent);
        assert_eq!(
            item,
            &get(&world, &id(item)).await,
            "the response is stored"
        );
    }
    assert_eq!(items[0]["categorized_by"]["agent"], agent);
    assert!(items[1]["categorized_by"].is_null());

    let updates = transaction_updates(&world).await;
    assert_eq!(updates.len(), 2, "one audit entry per operation");
    for update in updates {
        assert_eq!(
            update.actor,
            AuditActor::Member {
                member: world.alice.member,
                agent: Some(world.write_id),
            }
        );
    }
}

#[tokio::test]
async fn read_token_cannot_patch() {
    let world = world().await;
    let read = read_token(&world).await;
    let a = account(&world, "Checking").await;
    let first = transaction(&world, &a, "2026-10-01", -1000, "Grocery store").await;
    let path = format!("/api/v1/transactions/{first}");

    let (status, body) = call(
        &world.server,
        Method::PATCH,
        &path,
        &read,
        Some(json!({"review": "reviewed"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    let (status, _) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions/batch",
        &read,
        Some(json!({"operations": [{"id": first, "patch": {"review": "reviewed"}}]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = call(&world.server, Method::DELETE, &path, &read, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = call(
        &world.server,
        Method::POST,
        "/api/v1/transactions",
        &read,
        Some(
            json!({"account": a, "posted_on": "2026-10-02", "minor_units": -5,
                    "description": "Coffee"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Reading is allowed.
    let (status, _) = call(&world.server, Method::GET, &path, &read, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &world.server,
        Method::GET,
        "/api/v1/transactions",
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(get(&world, &first).await["review"], "unreviewed");
}

#[tokio::test]
async fn create_get_and_delete_manual_transaction() {
    let world = world().await;
    let a = account(&world, "Checking").await;
    let body = json!({"account": a, "posted_on": "2026-10-02", "minor_units": -450,
                      "description": "Coffee", "payee": "Corner cafe", "memo": "card",
                      "notes": "with Bob"});

    let first = send_raw(
        &world.server,
        request(
            Method::POST,
            "/api/v1/transactions",
            &world.write,
            Some(&body),
            Some("coffee-1"),
        ),
    )
    .await;
    assert_eq!(first.0, StatusCode::CREATED);
    let created: Value = serde_json::from_str(&first.1).unwrap();
    assert_eq!(
        created["amount"],
        json!({"minor_units": -450, "currency": "USD"})
    );
    assert_eq!(created["payee"], "Corner cafe");
    assert_eq!(created["memo"], "card");
    assert_eq!(created["notes"], "with Bob");
    assert_eq!(created["origin"], "manual");

    let retry = send_raw(
        &world.server,
        request(
            Method::POST,
            "/api/v1/transactions",
            &world.write,
            Some(&body),
            Some("coffee-1"),
        ),
    )
    .await;
    assert_eq!(retry.0, StatusCode::CREATED);
    assert_eq!(serde_json::from_str::<Value>(&retry.1).unwrap(), created);
    assert_eq!(
        listed(&world, "").await.len(),
        1,
        "the retry created nothing"
    );

    let transaction = id(&created);
    assert_eq!(get(&world, &transaction).await, created);
    let path = format!("/api/v1/transactions/{transaction}");
    let (status, _) = call(&world.server, Method::DELETE, &path, &world.write, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&world.server, Method::GET, &path, &world.write, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
