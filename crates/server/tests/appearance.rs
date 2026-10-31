//! Appearance: the household theme and each member's theme and colour
//! scheme, resolved into the look a page is drawn with.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::http::StatusCode;
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
    tx.set_household_theme(&theme("mochi")).await.unwrap();
    tx.commit().await.unwrap();
    let look = server.appearance.look(Some(&stored)).await;
    assert_eq!(look.theme.name, "mochi");
}
