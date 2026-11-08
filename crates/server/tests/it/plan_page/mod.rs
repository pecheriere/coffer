//! The plan page through the real router: its forms add, change, pause and delete planned
//! items that the API then answers, through the same service and audit, and every refusal is
//! fixed text.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod forms;
mod refusals;

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Role;
use coffer_domain::ids::MemberId;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::Value;

use crate::forecast::World;

const PAGE: &str = "/plan";

/// A browser signed in as `member`.
async fn signed_in(world: &World, member: MemberId) -> Browser {
    let session = world.server.sessions.create(member).await.unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    browser
}

/// A browser signed in as alice, the admin.
async fn admin(world: &World) -> Browser {
    signed_in(world, world.alice.member).await
}

/// A browser signed in as bob, a member who isn't an admin.
async fn member(world: &World) -> Browser {
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
    signed_in(world, bob).await
}

/// Posts `fields` to `path` with the anti-forgery token of the browser's session.
async fn post(browser: &mut Browser, path: &str, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(path, &all).await
}

/// Every planned item the API answers, paused ones too.
async fn items(world: &World) -> Vec<Value> {
    let uri = "/api/v1/plan-items?include_inactive=true";
    let (status, text) = world.send(Method::GET, uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Value = serde_json::from_str(&text).unwrap();
    list["items"].as_array().unwrap().clone()
}

/// The API's item named `name`.
async fn item(world: &World, name: &str) -> Value {
    items(world)
        .await
        .into_iter()
        .find(|item| item["name"] == name)
        .unwrap()
}

/// The audit actions about planned items, in alphabetical order: the test clock stands
/// still, so their times don't order them.
async fn audited(world: &World) -> Vec<String> {
    let events: Vec<AuditEvent> = world
        .server
        .store
        .list_audit(
            &AuditFilter {
                entity_type: Some("planned_item".into()),
                ..AuditFilter::default()
            },
            &PageRequest::default(),
        )
        .await
        .unwrap()
        .items;
    let mut actions: Vec<String> = events.into_iter().map(|event| event.action).collect();
    actions.sort();
    actions
}

/// The fields of a monthly bill of 1,234.56 USD named `name` in `category`, from the first
/// day of the worked example's month.
fn monthly_bill<'a>(name: &'a str, category: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("name", name),
        ("direction", "outflow"),
        ("category", category),
        ("amount", "1234.56"),
        ("currency", "USD"),
        ("type", "recurring"),
        ("every", "1"),
        ("unit", "months"),
        ("anchor_on", "2026-11-01"),
        ("end_on", ""),
        ("on", ""),
        ("start_on", ""),
        ("due_on", ""),
        ("spending", "bill"),
        ("account_hint", ""),
        ("notes", ""),
        ("active", "yes"),
    ]
}

/// `fields` with `field` set to `value`.
fn with<'a>(
    mut fields: Vec<(&'a str, &'a str)>,
    field: &'a str,
    value: &'a str,
) -> Vec<(&'a str, &'a str)> {
    fields.retain(|(name, _)| *name != field);
    fields.push((field, value));
    fields
}

/// `fields` with each of `changes` set.
fn with_all<'a>(
    mut fields: Vec<(&'a str, &'a str)>,
    changes: &[(&'a str, &'a str)],
) -> Vec<(&'a str, &'a str)> {
    for (field, value) in changes {
        fields = with(fields, field, value);
    }
    fields
}
