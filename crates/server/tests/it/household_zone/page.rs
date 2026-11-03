//! The time zone form on the household page.

use axum::http::{StatusCode, header};
use coffer_domain::identity::Role;
use coffer_server::testkit::browser::{Browser, Reply};
use secrecy::ExposeSecret as _;
use serde_json::Value;

use super::{AUCKLAND, LOS_ANGELES, LOS_ANGELES_DAY, world_at};
use crate::net_worth::World;

const HOUSEHOLD: &str = "/admin/household";
const SET_ZONE: &str = "/admin/household/timezone";

async fn alice(world: &World) -> Browser {
    let session = world
        .server
        .sessions
        .create(world.alice.member)
        .await
        .unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    browser
}

async fn bob(world: &World) -> Browser {
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let member = world
        .server
        .authenticator()
        .enroll(&invite, "bob", "Bob")
        .await;
    let session = world.server.sessions.create(member).await.unwrap();
    let mut browser = Browser::new(&world.server);
    browser.cookie = Some(session.expose_secret().to_owned());
    browser
}

async fn post_zone(browser: &mut Browser, fields: &[(&str, &str)]) -> Reply {
    let csrf = browser.get("/account").await.csrf();
    let mut all = vec![("csrf", csrf.as_str())];
    all.extend_from_slice(fields);
    browser.post_form(SET_ZONE, &all).await
}

async fn setting(world: &World) -> Value {
    let (_, text) = world
        .send(
            axum::http::Method::GET,
            "/api/v1/household/settings",
            &world.admin,
            None,
        )
        .await;
    let settings: Value = serde_json::from_str(&text).unwrap();
    settings["timezone_setting"].clone()
}

#[tokio::test]
async fn an_admin_chooses_and_clears_it_and_is_told_what_it_does() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    let mut admin = alice(&world).await;

    let form = admin.get(HOUSEHOLD).await.body;
    assert!(form.contains(&format!("action=\"{SET_ZONE}\"")));
    assert!(
        form.contains(&format!("Server default ({LOS_ANGELES})")),
        "names the default"
    );
    assert!(
        form.contains("<option value=\"\" selected>"),
        "the default is what is chosen"
    );
    assert!(
        form.contains("Days already recorded stay as they are"),
        "says what a change leaves alone"
    );
    assert!(form.contains("Only new days use the new zone"));

    let reply = post_zone(&mut admin, &[("timezone", AUCKLAND)]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.header(header::LOCATION), HOUSEHOLD);
    assert_eq!(setting(&world).await, AUCKLAND);
    let events = world.settings_events().await;
    assert_eq!(events.len(), 1, "audited by the same service");
    assert_eq!(events[0].action, "household.settings");
    let form = admin.get(HOUSEHOLD).await.body;
    assert!(form.contains(&format!("<option value=\"{AUCKLAND}\" selected>")));

    let reply = post_zone(&mut admin, &[("timezone", "")]).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(setting(&world).await, Value::Null);
    assert_eq!(world.settings_events().await.len(), 2);
}

#[tokio::test]
async fn an_unknown_zone_is_refused_with_a_reason() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    let mut admin = alice(&world).await;

    let reply = post_zone(&mut admin, &[("timezone", "<b>Mars</b>")]).await;

    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("IANA name"));
    assert!(!reply.body.contains("Mars"), "does not echo");
    assert_eq!(setting(&world).await, Value::Null);
}

#[tokio::test]
async fn a_member_is_refused_whatever_is_sent() {
    let world = world_at(LOS_ANGELES, LOS_ANGELES_DAY).await;
    let mut member = bob(&world).await;

    for fields in [
        &[("timezone", AUCKLAND)][..],
        &[("timezone", "")][..],
        &[("timezone", AUCKLAND), ("extra", "1")][..],
        &[][..],
    ] {
        let reply = post_zone(&mut member, fields).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{fields:?}");
    }

    assert_eq!(setting(&world).await, Value::Null);
    assert_eq!(world.settings_events().await.len(), 0);
}
