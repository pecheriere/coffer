//! The categories page through the real router: its forms change what the API then answers,
//! through the same service and audit, and every refusal is fixed text.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::MemberId;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

const PAGE: &str = "/categories";
const GROUPS: &str = "/category-groups";

/// A household with an admin, alice, a token that reads what the API answers and one that
/// writes through it.
pub(crate) struct World {
    pub(crate) server: TestServer,
    pub(crate) alice: Actor,
    reader: SecretToken,
    writer: SecretToken,
}

pub(crate) async fn world() -> World {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, reader) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("reader").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let (_, writer) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("writer").unwrap(),
            Scope::Write,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    World {
        server,
        alice,
        reader,
        writer,
    }
}

impl World {
    /// A browser signed in as alice.
    pub(crate) async fn admin(&self) -> Browser {
        self.signed_in(self.alice.member).await
    }

    /// A browser signed in as bob, a member who isn't an admin.
    pub(crate) async fn member(&self) -> Browser {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let bob = self
            .server
            .authenticator()
            .enroll(&invite, "bob", "Bob")
            .await;
        self.signed_in(bob).await
    }

    async fn signed_in(&self, member: MemberId) -> Browser {
        let session = self.server.sessions.create(member).await.unwrap();
        let mut browser = Browser::new(&self.server);
        browser.cookie = Some(session.expose_secret().to_owned());
        browser
    }

    /// What `GET path` on the API answers.
    pub(crate) async fn api(&self, path: &str) -> Value {
        let request = Request::builder()
            .uri(path)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.reader.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
        let reply = Browser::new(&self.server).send(request).await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.json()
    }

    /// Creates a monthly planned outflow on `category` through the API.
    async fn plan_item(&self, category: &str) {
        let body = json!({
            "name": "Rent",
            "category": category,
            "amount": {"minor_units": 120_000, "currency": "USD"},
            "direction": "outflow",
            "schedule": {"type": "recurring", "every": 1, "unit": "months",
                         "anchor_on": "2027-01-01"},
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/plan-items")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.writer.expose_secret()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let reply = Browser::new(&self.server).send(request).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }

    /// The API's category groups.
    pub(crate) async fn groups(&self) -> Vec<Value> {
        self.api("/api/v1/categories").await["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// The audit events about `entity_type`.
    pub(crate) async fn audit(&self, entity_type: &str) -> Vec<AuditEvent> {
        self.server
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
}

/// Posts `fields` to `path` with the anti-forgery token of the browser's session.
pub(crate) async fn post(browser: &mut Browser, path: &str, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(path, &all).await
}

/// Creates a group from the page; its id, read back from the API.
pub(crate) async fn add_group(world: &World, browser: &mut Browser, name: &str) -> String {
    let reply = post(browser, GROUPS, &[("name", name)]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let group = world
        .groups()
        .await
        .into_iter()
        .find(|group| group["name"] == name)
        .unwrap();
    let id = group["id"].as_str().unwrap().to_owned();
    assert_eq!(
        reply.header(header::LOCATION),
        format!("{PAGE}#group-{id}"),
        "back to the group"
    );
    id
}

/// Creates a category from the page; its id, read back from the API.
pub(crate) async fn add_category(
    world: &World,
    browser: &mut Browser,
    group: &str,
    name: &str,
    kind: &str,
) -> String {
    let fields = [("group", group), ("name", name), ("kind", kind)];
    let reply = post(browser, PAGE, &fields).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    category(world, name).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The API's category named `name`.
async fn category(world: &World, name: &str) -> Value {
    world
        .groups()
        .await
        .iter()
        .flat_map(|group| group["categories"].as_array().unwrap().clone())
        .find(|category| category["name"] == name)
        .unwrap()
}

#[tokio::test]
async fn an_empty_household_is_offered_a_group_first() {
    let world = world().await;
    let mut admin = world.admin().await;

    let reply = admin.get(PAGE).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("No categories yet"));
    assert!(reply.body.contains(&format!("action=\"{GROUPS}\"")));
    assert!(
        !reply.body.contains("Add a category"),
        "no group to add it to"
    );
    assert!(
        reply
            .body
            .contains("href=\"/categories\" aria-current=\"page\""),
        "in the navigation"
    );
}

#[tokio::test]
async fn groups_and_categories_are_created_changed_and_archived_from_the_page() {
    let world = world().await;
    let mut admin = world.admin().await;

    let home = add_group(&world, &mut admin, "Home").await;
    let food = add_group(&world, &mut admin, "Food").await;
    let rent = add_category(&world, &mut admin, &home, "Rent", "expense").await;
    let shown = admin.get(PAGE).await.body;
    assert!(shown.contains("Rent"));
    assert!(shown.contains("Spending"), "the kind in words");
    assert!(shown.contains("Add a category"));

    let path = format!("{PAGE}/{rent}");
    let fields = [
        ("name", "Housing"),
        ("kind", "income"),
        ("group", food.as_str()),
        ("position", "7"),
    ];
    assert_eq!(
        post(&mut admin, &path, &fields).await.status,
        StatusCode::SEE_OTHER
    );
    let changed = category(&world, "Housing").await;
    assert_eq!(changed["kind"], "income");
    assert_eq!(changed["group"], food.as_str());
    assert_eq!(changed["position"], 7);

    let archive = format!("{path}/archive");
    let reply = post(&mut admin, &archive, &[("archived", "yes")]).await;
    assert_eq!(
        reply.header(header::LOCATION),
        format!("{PAGE}#group-{food}")
    );
    assert_eq!(category(&world, "Housing").await["archived"], true);
    assert!(admin.get(PAGE).await.body.contains("Restore"));
    post(&mut admin, &archive, &[("archived", "no")]).await;
    assert_eq!(category(&world, "Housing").await["archived"], false);

    let group_path = format!("{GROUPS}/{home}");
    let fields = [("name", "House"), ("position", "3")];
    assert_eq!(
        post(&mut admin, &group_path, &fields).await.status,
        StatusCode::SEE_OTHER
    );
    post(
        &mut admin,
        &format!("{group_path}/archive"),
        &[("archived", "yes")],
    )
    .await;
    let house = world
        .groups()
        .await
        .into_iter()
        .find(|group| group["id"] == home.as_str())
        .unwrap();
    assert_eq!(house["name"], "House");
    assert_eq!(house["position"], 3);
    assert_eq!(house["archived"], true);
    assert!(
        admin
            .get(PAGE)
            .await
            .body
            .contains("Every category in an archived group")
    );

    let groups: Vec<String> = world
        .audit("category_group")
        .await
        .iter()
        .map(|event| event.action.clone())
        .collect();
    assert_eq!(
        groups.len(),
        4,
        "two creates, a change and an archive: {groups:?}"
    );
    let categories = world.audit("category").await;
    assert_eq!(
        categories.len(),
        4,
        "a create, a change, an archive and a restore"
    );
    let by_alice = AuditActor::Member {
        member: world.alice.member,
        agent: None,
    };
    assert!(
        categories.iter().all(|event| event.actor == by_alice),
        "as alice, from her session"
    );
}

#[tokio::test]
async fn a_member_manages_categories_as_the_api_lets_them() {
    let world = world().await;
    let mut member = world.member().await;

    let page = member.get(PAGE).await.body;
    assert!(
        page.contains(&format!("action=\"{GROUPS}\"")),
        "the API lets a member write"
    );

    let group = add_group(&world, &mut member, "Kids").await;
    add_category(&world, &mut member, &group, "School", "expense").await;
    assert_eq!(world.audit("category").await.len(), 1);
}

#[tokio::test]
async fn a_category_a_planned_item_uses_keeps_its_kind() {
    let world = world().await;
    let mut admin = world.admin().await;
    let home = add_group(&world, &mut admin, "Home").await;
    let rent = add_category(&world, &mut admin, &home, "Rent", "expense").await;
    world.plan_item(&rent).await;
    let audited = world.audit("category").await.len();
    let lock =
        "Its kind can't change while a planned item uses it or spending in it is linked to a goal.";
    assert!(
        admin.get(PAGE).await.body.contains(lock),
        "the form says so first"
    );

    let fields = [
        ("name", "Rent"),
        ("kind", "income"),
        ("group", home.as_str()),
        ("position", "0"),
    ];
    let reply = post(&mut admin, &format!("{PAGE}/{rent}"), &fields).await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        reply.body.contains(lock),
        "the same words as the form's hint: {}",
        reply.body
    );
    assert_eq!(category(&world, "Rent").await["kind"], "expense");
    assert_eq!(
        world.audit("category").await.len(),
        audited,
        "nothing audited"
    );
}

/// Where a form goes, what it sends, and what the refusal must say.
type Refusal<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);

#[tokio::test]
async fn refusals_are_fixed_text_that_never_echoes_the_input() {
    let world = world().await;
    let mut admin = world.admin().await;
    let home = add_group(&world, &mut admin, "Home").await;
    let rent = add_category(&world, &mut admin, &home, "Rent", "expense").await;
    let marker = format!("<b>MARKER</b>{}", "x".repeat(100));
    let path = format!("{PAGE}/{rent}");
    let group_path = format!("{GROUPS}/{home}");
    let unknown_group = "0190f5f0-0000-7000-8000-000000000000";

    let cases: [Refusal<'_>; 11] = [
        (GROUPS, vec![("name", &marker)], "1 to 100 characters"),
        (
            PAGE,
            vec![("group", &home), ("name", &marker), ("kind", "expense")],
            "1 to 100 characters",
        ),
        (
            PAGE,
            vec![("group", "MARKER"), ("name", "Food"), ("kind", "expense")],
            "not a group the form offers",
        ),
        (
            PAGE,
            vec![
                ("group", unknown_group),
                ("name", "Food"),
                ("kind", "expense"),
            ],
            "does not exist",
        ),
        (
            PAGE,
            vec![("group", &home), ("name", "Food"), ("kind", "MARKER")],
            "not a kind the form offers",
        ),
        (
            &path,
            vec![
                ("name", "Rent"),
                ("kind", "expense"),
                ("group", &home),
                ("position", "MARKER"),
            ],
            "whole number",
        ),
        (
            &path,
            vec![
                ("name", "Rent"),
                ("kind", "expense"),
                ("group", &home),
                ("position", "10001"),
            ],
            "at most 10000",
        ),
        (
            &group_path,
            vec![("name", "Home"), ("position", "MARKER")],
            "whole number",
        ),
        (
            &group_path,
            vec![("name", "Home"), ("position", "10001")],
            "at most 10000",
        ),
        (
            &format!("{path}/archive"),
            vec![("archived", "MARKER")],
            "not an answer the form offers",
        ),
        (
            &format!("{group_path}/archive"),
            vec![("archived", "MARKER")],
            "not an answer the form offers",
        ),
    ];
    for (path, fields, reason) in cases {
        let reply = post(&mut admin, path, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{reason}");
        assert!(reply.body.contains(reason), "{reason}");
        assert!(!reply.body.contains("MARKER"), "does not echo");
    }

    let reply = post(&mut admin, GROUPS, &[("name", "Food"), ("extra", "1")]).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a forged field fails the form"
    );
    let reply = admin
        .post_form(GROUPS, &[("csrf", "forged"), ("name", "Food")])
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "a forged token");
    assert_eq!(world.groups().await.len(), 1, "nothing written");
    assert_eq!(category(&world, "Rent").await["position"], 0);
    assert_eq!(world.audit("category_group").await.len(), 1);
    assert_eq!(world.audit("category").await.len(), 1);
}

#[tokio::test]
async fn unknown_ids_are_not_found_and_not_echoed() {
    let world = world().await;
    let mut admin = world.admin().await;
    let home = add_group(&world, &mut admin, "Home").await;
    add_category(&world, &mut admin, &home, "Rent", "expense").await;

    let unknown = "0190f5f0-0000-7000-8000-00000000abcd";
    let category_form = [
        ("name", "Rent"),
        ("kind", "expense"),
        ("group", home.as_str()),
        ("position", "0"),
    ];
    let group_form = [("name", "Home"), ("position", "0")];
    let archive = [("archived", "yes")];
    let missing: [(String, &[(&str, &str)]); 5] = [
        (format!("{PAGE}/{unknown}"), &category_form),
        (format!("{PAGE}/{unknown}/archive"), &archive),
        (format!("{GROUPS}/{unknown}"), &group_form),
        (format!("{GROUPS}/{unknown}/archive"), &archive),
        (format!("{PAGE}/{unknown}-not-an-id/archive"), &archive),
    ];
    for (path, fields) in missing {
        let reply = post(&mut admin, &path, fields).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert!(!reply.body.contains(unknown), "does not echo {path}");
    }

    assert_eq!(
        world.audit("category_group").await.len(),
        1,
        "nothing written"
    );
    assert_eq!(world.audit("category").await.len(), 1, "nothing written");
}
