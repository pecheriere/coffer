//! Appearance: the household theme and each member's theme and colour
//! scheme, resolved into the look a page is drawn with.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::http::{StatusCode, header};
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, ColorScheme, IdentityError, Role, Scope, ThemeName, Via};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::policy::{Action, Denied, DenyReason};
use coffer_server::auth::AuthError;
use coffer_server::testkit::TestServer;
use coffer_server::testkit::browser::{Browser, with_admin};
use coffer_server::themes::{self, DEFAULT};
use coffer_store::{AuditEvent, AuditFilter, Member, PageRequest};
use serde_json::json;

fn session(member: MemberId, role: Role) -> Actor {
    Actor {
        member,
        role,
        via: Via::Session,
    }
}

fn theme(name: &str) -> ThemeName {
    ThemeName::parse(name).unwrap()
}

/// A server with admin "alice" and member "bob", both enrolled.
async fn household() -> (TestServer, MemberId, MemberId) {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    let alice = server
        .authenticator()
        .enroll(&setup, "alice", "Alice")
        .await;
    let invite = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let bob = server.authenticator().enroll(&invite, "bob", "Bob").await;
    (server, alice, bob)
}

async fn member(server: &TestServer, id: MemberId) -> Member {
    server.store.get_member(id).await.unwrap()
}

async fn events(server: &TestServer, action: &str) -> Vec<AuditEvent> {
    server
        .store
        .list_audit(
            &AuditFilter::default(),
            &PageRequest {
                after: None,
                limit: 200,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|event| event.action == action)
        .collect()
}

#[tokio::test]
async fn new_member_sees_the_household_default() {
    let (server, _alice, bob) = household().await;
    let bob = member(&server, bob).await;
    let look = server.appearance.look(Some(&bob)).await;
    assert_eq!(look.theme.name, DEFAULT.name);
    assert_eq!(look.scheme, ColorScheme::System);
    let look = server.appearance.look(None).await;
    assert_eq!(look.theme.name, DEFAULT.name);
    assert_eq!(look.scheme, ColorScheme::System);
    assert_eq!(server.appearance.household_theme().await.name, DEFAULT.name);
}

#[tokio::test]
async fn member_choice_is_stored_and_audited() {
    let (server, _alice, bob) = household().await;
    let actor = session(bob, Role::Member);
    server
        .appearance
        .set_member(&actor, None, ColorScheme::Dark)
        .await
        .unwrap();

    let stored = member(&server, bob).await;
    assert_eq!(stored.theme, None);
    assert_eq!(stored.color_scheme, ColorScheme::Dark);
    assert_eq!(
        server.appearance.look(Some(&stored)).await.scheme,
        ColorScheme::Dark
    );
    let recorded = events(&server, "member.appearance").await;
    assert_eq!(recorded.len(), 1);
    let event = &recorded[0];
    assert_eq!(
        event.actor,
        AuditActor::Member {
            member: bob,
            agent: None
        }
    );
    assert_eq!(event.entity_type, "member");
    assert_eq!(event.entity_id, bob.to_string());
    assert_eq!(
        event.changes,
        json!({"color_scheme": {"from": "system", "to": "dark"}})
    );

    // The same choice again changes nothing and records nothing.
    server
        .appearance
        .set_member(&actor, None, ColorScheme::Dark)
        .await
        .unwrap();
    assert_eq!(events(&server, "member.appearance").await.len(), 1);

    // Choosing a theme records only the theme.
    server
        .appearance
        .set_member(&actor, Some(theme("mochi")), ColorScheme::Dark)
        .await
        .unwrap();
    assert_eq!(member(&server, bob).await.theme, Some(theme("mochi")));
    let recorded = events(&server, "member.appearance").await;
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        recorded[0].changes,
        json!({"theme": {"from": null, "to": "mochi"}})
    );
}

#[tokio::test]
async fn unregistered_theme_is_rejected() {
    let (server, alice, bob) = household().await;
    let err = server
        .appearance
        .set_member(
            &session(bob, Role::Member),
            Some(theme("no-such-theme")),
            ColorScheme::Dark,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AuthError::Invalid(IdentityError::UnknownTheme)
    ));
    let stored = member(&server, bob).await;
    assert_eq!(stored.theme, None);
    assert_eq!(stored.color_scheme, ColorScheme::System);

    let err = server
        .appearance
        .set_household_theme(&session(alice, Role::Admin), theme("no-such-theme"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AuthError::Invalid(IdentityError::UnknownTheme)
    ));
    assert_eq!(server.store.household_theme().await.unwrap(), None);
    assert_eq!(
        events(&server, "member.appearance").await,
        Vec::<AuditEvent>::new()
    );
    assert_eq!(
        events(&server, "household.theme").await,
        Vec::<AuditEvent>::new()
    );
}

#[tokio::test]
async fn household_theme_requires_admin() {
    let (server, alice, bob) = household().await;
    let err = server
        .appearance
        .set_household_theme(&session(bob, Role::Member), theme("mochi"))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AuthError::Denied(Denied {
            action: Action::ManageHousehold,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    ));
    assert_eq!(server.store.household_theme().await.unwrap(), None);
    assert_eq!(
        events(&server, "household.theme").await,
        Vec::<AuditEvent>::new()
    );

    let admin = session(alice, Role::Admin);
    server
        .appearance
        .set_household_theme(&admin, theme("mochi"))
        .await
        .unwrap();
    assert_eq!(
        server.store.household_theme().await.unwrap(),
        Some(theme("mochi"))
    );
    let recorded = events(&server, "household.theme").await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].actor,
        AuditActor::Member {
            member: alice,
            agent: None
        }
    );
    assert_eq!(recorded[0].entity_type, "household");
    assert_eq!(recorded[0].entity_id, "household");
    assert_eq!(recorded[0].changes, json!({"from": null, "to": "mochi"}));

    // Setting the same theme again records nothing.
    server
        .appearance
        .set_household_theme(&admin, theme("mochi"))
        .await
        .unwrap();
    assert_eq!(events(&server, "household.theme").await.len(), 1);
}

#[tokio::test]
async fn read_token_cannot_change_preferences() {
    let (server, alice, bob) = household().await;
    for (member, role) in [(bob, Role::Member), (alice, Role::Admin)] {
        let agent = Actor {
            member,
            role,
            via: Via::Agent {
                token: AgentTokenId::new(),
                scope: Scope::Read,
            },
        };
        let err = server
            .appearance
            .set_member(&agent, None, ColorScheme::Dark)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Denied(Denied {
                action: Action::ManageOwnPreferences,
                reason: DenyReason::InsufficientScope {
                    needed: Scope::Write
                },
            })
        ));
        assert_eq!(
            member_scheme(&server, member).await,
            ColorScheme::System,
            "{role:?}"
        );
    }
    assert_eq!(
        events(&server, "member.appearance").await,
        Vec::<AuditEvent>::new()
    );
}

async fn member_scheme(server: &TestServer, id: MemberId) -> ColorScheme {
    member(server, id).await.color_scheme
}

#[tokio::test]
async fn write_token_changes_only_its_own_member() {
    let (server, alice, bob) = household().await;
    let token = AgentTokenId::new();
    let agent = Actor {
        member: bob,
        role: Role::Member,
        via: Via::Agent {
            token,
            scope: Scope::Write,
        },
    };
    server
        .appearance
        .set_member(&agent, None, ColorScheme::Light)
        .await
        .unwrap();
    assert_eq!(member_scheme(&server, bob).await, ColorScheme::Light);
    assert_eq!(member_scheme(&server, alice).await, ColorScheme::System);
    let recorded = events(&server, "member.appearance").await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].actor,
        AuditActor::Member {
            member: bob,
            agent: Some(token)
        }
    );
}

#[tokio::test]
async fn member_with_a_corrupt_theme_still_signs_in() {
    let (server, _browser, mut auth) = with_admin().await;
    let alice = server
        .store
        .list_members()
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    server.corrupt_member_theme(alice).await;

    let mut browser = Browser::new(&server);
    let signed_in = browser.login(&mut auth, "alice").await;
    assert_eq!(signed_in.status, StatusCode::OK, "{}", signed_in.body);
    let account = browser.get("/account").await;
    assert_eq!(account.status, StatusCode::OK, "{}", account.body);
    assert!(account.body.contains("Alice"));
    assert_eq!(member(&server, alice).await.theme, None);
}

#[tokio::test]
async fn member_with_a_corrupt_color_scheme_still_signs_in() {
    let (server, _browser, mut auth) = with_admin().await;
    let alice = server
        .store
        .list_members()
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    server.corrupt_member_color_scheme(alice).await;

    let mut browser = Browser::new(&server);
    let signed_in = browser.login(&mut auth, "alice").await;
    assert_eq!(signed_in.status, StatusCode::OK, "{}", signed_in.body);
    let account = browser.get("/account").await;
    assert_eq!(account.status, StatusCode::OK, "{}", account.body);
    assert!(account.body.contains("Alice"));
    assert_eq!(member_scheme(&server, alice).await, ColorScheme::System);
}

#[tokio::test]
async fn unknown_stored_theme_falls_back() {
    let (server, _alice, bob) = household().await;
    let retired = theme("retired");
    assert!(themes::find(retired.as_str()).is_none());
    let mut tx = server.store.begin().await.unwrap();
    tx.set_household_theme(&retired).await.unwrap();
    tx.set_member_appearance(bob, Some(&retired), ColorScheme::Light)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let stored = member(&server, bob).await;
    let look = server.appearance.look(Some(&stored)).await;
    assert_eq!(look.theme.name, DEFAULT.name);
    assert_eq!(look.scheme, ColorScheme::Light);
    assert_eq!(server.appearance.household_theme().await.name, DEFAULT.name);

    let mut tx = server.store.begin().await.unwrap();
    tx.set_household_theme(&theme("fjord")).await.unwrap();
    tx.commit().await.unwrap();
    let look = server.appearance.look(Some(&stored)).await;
    assert_eq!(look.theme.name, "fjord");
}

// Pages: the member's appearance card and the household page.

/// Admin "alice" and member "bob", each signed in over HTTP in a browser.
async fn browsers() -> (TestServer, Browser, Browser, MemberId) {
    let (server, alice, _) = with_admin().await;
    let actor = server
        .sessions
        .resolve(alice.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let invite = server
        .identity
        .create_invite(&actor, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    let done = bob.enroll(&mut bob_key, &invite, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let bob_id = server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap()
        .member;
    (server, alice, bob, bob_id)
}

/// Whether the radio `name`=`value` in `body` is checked.
///
/// Panics if the page has no such radio.
fn checked(body: &str, name: &str, value: &str) -> bool {
    let needle = format!("type=\"radio\" name=\"{name}\" value=\"{value}\"");
    let at = body
        .find(&needle)
        .unwrap_or_else(|| panic!("no radio {name}={value:?} in page:\n{body}"));
    let start = body[..at].rfind("<input").unwrap();
    let end = at + body[at..].find('>').unwrap();
    body[start..end].contains(" checked")
}

const FORGED: &str = "x\"><script>";

#[tokio::test]
async fn account_page_offers_every_theme_and_scheme() {
    let (_server, _alice, mut bob, _) = browsers().await;
    let page = bob.get("/account").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains("<h2>Appearance</h2>"));
    assert!(body.contains("action=\"/account/appearance\""));
    assert!(body.contains("Household default (Mochi)"));
    assert!(checked(body, "theme", ""));
    for theme in themes::THEMES {
        assert!(!checked(body, "theme", theme.name), "{}", theme.name);
        assert!(body.contains(theme.label));
    }
    assert!(checked(body, "color_scheme", "system"));
    assert!(!checked(body, "color_scheme", "light"));
    assert!(!checked(body, "color_scheme", "dark"));
    for label in ["Match this device", "Light", "Dark", "Save appearance"] {
        assert!(body.contains(label), "missing {label}");
    }
}

#[tokio::test]
async fn forcing_a_scheme_is_rendered_without_script() {
    let (_server, _alice, mut bob, _) = browsers().await;
    let csrf = bob.get("/account").await.csrf();
    let saved = bob
        .post_form(
            "/account/appearance",
            &[("csrf", &csrf), ("theme", ""), ("color_scheme", "dark")],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.header(header::LOCATION), "/account");

    let page = bob.get("/account").await;
    assert!(
        page.body
            .contains("<html lang=\"en\" data-scheme=\"dark\">")
    );
    assert!(page.body.contains("content=\"dark\""));
    assert!(checked(&page.body, "color_scheme", "dark"));
    assert!(!checked(&page.body, "color_scheme", "system"));

    let saved = bob
        .post_form(
            "/account/appearance",
            &[
                ("csrf", &csrf),
                ("theme", "mochi"),
                ("color_scheme", "system"),
            ],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let page = bob.get("/account").await;
    assert!(!page.body.contains("data-scheme"));
    assert!(page.body.contains("content=\"light dark\""));
    assert!(checked(&page.body, "theme", "mochi"));
    assert!(!checked(&page.body, "theme", ""));
}

#[tokio::test]
async fn error_pages_wear_the_members_scheme() {
    let (_server, _alice, mut bob, _) = browsers().await;
    let csrf = bob.get("/account").await.csrf();
    let saved = bob
        .post_form(
            "/account/appearance",
            &[("csrf", &csrf), ("theme", ""), ("color_scheme", "dark")],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    let missing = bob.get("/nope").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(missing.body.contains("illustration-error"));
    assert!(missing.body.contains("data-scheme=\"dark\""));
    assert!(missing.body.contains("content=\"dark\""));
}

#[tokio::test]
async fn appearance_rejects_unknown_values() {
    let (server, _alice, mut bob, bob_id) = browsers().await;
    let csrf = bob.get("/account").await.csrf();
    let long = "a".repeat(10_000);
    let forged: [&[(&str, &str)]; 5] = [
        &[("theme", FORGED), ("color_scheme", "dark")],
        &[("theme", "nope"), ("color_scheme", "dark")],
        &[("theme", &long), ("color_scheme", "dark")],
        &[("theme", ""), ("color_scheme", "purple")],
        &[("theme", ""), ("color_scheme", "dark"), ("extra", "1")],
    ];
    for fields in forged {
        let mut all = vec![("csrf", csrf.as_str())];
        all.extend_from_slice(fields);
        let reply = bob.post_form("/account/appearance", &all).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{fields:?}: {}",
            reply.body
        );
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains("<script>\""), "{fields:?} echoed");
        assert!(!reply.body.contains(&long));
    }
    let unsigned = bob
        .post_form(
            "/account/appearance",
            &[("theme", ""), ("color_scheme", "dark")],
        )
        .await;
    assert_eq!(unsigned.status, StatusCode::FORBIDDEN);

    let stored = member(&server, bob_id).await;
    assert_eq!(stored.theme, None);
    assert_eq!(stored.color_scheme, ColorScheme::System);
    assert_eq!(
        events(&server, "member.appearance").await,
        Vec::<AuditEvent>::new()
    );
}

#[tokio::test]
async fn household_page_is_admin_only() {
    let (server, mut alice, mut bob, _) = browsers().await;
    let bob_account = bob.get("/account").await;
    assert!(!bob_account.body.contains("href=\"/admin/household\""));
    assert_eq!(
        bob.get("/admin/household").await.status,
        StatusCode::FORBIDDEN
    );
    let refused = bob
        .post_form(
            "/admin/household",
            &[("csrf", &bob_account.csrf()), ("theme", "mochi")],
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(server.store.household_theme().await.unwrap(), None);

    let page = alice.get("/admin/household").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("<h1>Household</h1>"));
    assert!(
        page.body.contains(
            "The theme everyone sees unless they choose their own on their account page."
        )
    );
    assert!(page.body.contains("Save household theme"));
    assert!(checked(&page.body, "theme", "mochi"));
    assert!(
        page.body
            .contains("href=\"/admin/household\" aria-current=\"page\"")
    );

    let saved = alice
        .post_form(
            "/admin/household",
            &[("csrf", &page.csrf()), ("theme", "mochi")],
        )
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.header(header::LOCATION), "/admin/household");
    assert_eq!(
        server.store.household_theme().await.unwrap(),
        Some(theme("mochi"))
    );
    let recorded = events(&server, "household.theme").await;
    assert_eq!(recorded.len(), 1);
    let alice_id = server
        .sessions
        .resolve(alice.cookie.as_deref().unwrap())
        .await
        .unwrap()
        .member;
    assert_eq!(
        recorded[0].actor,
        AuditActor::Member {
            member: alice_id,
            agent: None
        }
    );
    assert_eq!(recorded[0].changes, json!({"from": null, "to": "mochi"}));
}

#[tokio::test]
async fn household_form_rejects_forged_values() {
    let (server, mut alice, _bob, _) = browsers().await;
    let csrf = alice.get("/admin/household").await.csrf();
    let forged: [&[(&str, &str)]; 4] = [
        &[("theme", FORGED)],
        &[("theme", "nope")],
        &[("theme", "")],
        &[("theme", "mochi"), ("extra", "1")],
    ];
    for fields in forged {
        let mut all = vec![("csrf", csrf.as_str())];
        all.extend_from_slice(fields);
        let reply = alice.post_form("/admin/household", &all).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{fields:?}: {}",
            reply.body
        );
        assert!(reply.header(header::CONTENT_TYPE).starts_with("text/html"));
        assert!(!reply.body.contains("<script>\""), "{fields:?} echoed");
    }
    let unsigned = alice
        .post_form("/admin/household", &[("theme", "mochi")])
        .await;
    assert_eq!(unsigned.status, StatusCode::FORBIDDEN);

    assert_eq!(server.store.household_theme().await.unwrap(), None);
    assert_eq!(
        events(&server, "household.theme").await,
        Vec::<AuditEvent>::new()
    );
}

#[tokio::test]
async fn account_page_survives_a_retired_theme() {
    let (server, _alice, mut bob, bob_id) = browsers().await;
    let mut tx = server.store.begin().await.unwrap();
    tx.set_member_appearance(bob_id, Some(&theme("retired")), ColorScheme::System)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let page = bob.get("/account").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(checked(&page.body, "theme", ""));
    assert!(
        page.body
            .contains(&format!("href=\"{}\"", DEFAULT.stylesheet.url))
    );
}

/// The URL of the registered theme `name`'s stylesheet.
fn stylesheet(name: &str) -> &'static str {
    themes::find(name).unwrap().stylesheet.url
}

fn links(body: &str, name: &str) -> bool {
    body.contains(&format!("href=\"{}\"", stylesheet(name)))
}

#[tokio::test]
async fn household_default_applies_to_everyone_without_a_choice() {
    let (server, mut alice, mut bob, _) = browsers().await;
    let csrf = alice.get("/admin/household").await.csrf();
    let saved = alice
        .post_form("/admin/household", &[("csrf", &csrf), ("theme", "fjord")])
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    let mut anonymous = Browser::new(&server);
    for path in ["/login", "/nope"] {
        let page = anonymous.get(path).await;
        assert!(links(&page.body, "fjord"), "{path}: {}", page.body);
        assert!(!links(&page.body, "mochi"), "{path}");
    }
    let account = bob.get("/account").await;
    assert!(links(&account.body, "fjord"));
    assert!(!links(&account.body, "mochi"));
    assert!(account.body.contains("Household default (Fjord)"));
    assert!(checked(&account.body, "theme", ""));
}

#[tokio::test]
async fn member_override_beats_the_household_default() {
    let (_server, mut alice, mut bob, _) = browsers().await;
    let csrf = alice.get("/admin/household").await.csrf();
    let saved = alice
        .post_form("/admin/household", &[("csrf", &csrf), ("theme", "fjord")])
        .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    let csrf = bob.get("/account").await.csrf();
    let chosen = bob
        .post_form(
            "/account/appearance",
            &[
                ("csrf", &csrf),
                ("theme", "mochi"),
                ("color_scheme", "system"),
            ],
        )
        .await;
    assert_eq!(chosen.status, StatusCode::SEE_OTHER, "{}", chosen.body);

    for path in ["/account", "/nope"] {
        let page = bob.get(path).await;
        assert!(links(&page.body, "mochi"), "{path}");
        assert!(!links(&page.body, "fjord"), "{path}");
    }
    let admin = alice.get("/admin/household").await;
    assert!(links(&admin.body, "fjord"));
    assert!(!links(&admin.body, "mochi"));
}
