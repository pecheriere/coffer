//! Sessions and CSRF: lifetimes, resolution against the current member,
//! logout and the cookie.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::ids::MemberId;
use coffer_domain::time::Clock as _;
use coffer_server::auth::{AuthError, session_cookie};
use coffer_server::secrets::{SecretToken, TokenKind};
use coffer_server::testkit::{TestServer, event_signal};
use coffer_store::{AuditEvent, AuditFilter, PageRequest, SessionRecord};
use jiff::SignedDuration;
use secrecy::ExposeSecret as _;

/// A server with admin "alice", who is signed in.
async fn signed_in() -> (TestServer, MemberId, SecretToken) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut auth = server.authenticator();
    let alice = auth.enroll(&link, "alice", "Alice").await;
    let (_, cookie) = auth.login_with_session("alice").await;
    (server, alice, cookie)
}

async fn audit(server: &TestServer) -> Vec<AuditEvent> {
    server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items
}

fn advance(server: &TestServer, by: SignedDuration) {
    server.clock.advance(by);
}

#[tokio::test]
async fn session_resolves_to_member_actor() {
    let (server, alice, cookie) = signed_in().await;

    let actor = server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();

    assert_eq!(
        actor,
        Actor {
            member: alice,
            role: Role::Admin,
            via: Via::Session
        }
    );
}

#[tokio::test]
async fn unknown_cookie_is_unauthenticated() {
    let (server, _, _) = signed_in().await;

    let err = server.sessions.resolve("not-a-session").await.unwrap_err();

    assert!(matches!(err, AuthError::Unauthenticated));
}

#[tokio::test]
async fn session_idle_boundary() {
    let (server, _, cookie) = signed_in().await;

    advance(
        &server,
        server.config.session_idle - SignedDuration::from_secs(1),
    );
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();

    let (server, _, cookie) = signed_in().await;
    advance(&server, server.config.session_idle);
    let err = server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated));
}

#[tokio::test]
async fn activity_extends_idle_not_absolute() {
    let (server, _, cookie) = signed_in().await;
    let six_days = SignedDuration::from_hours(6 * 24);

    // Day 6, 12, 18, 24: each visit slides the idle expiry.
    for _ in 0..4 {
        advance(&server, six_days);
        server
            .sessions
            .resolve(cookie.expose_secret())
            .await
            .unwrap();
    }

    // Day 29 still lives, day 30 is the absolute limit.
    advance(&server, SignedDuration::from_hours(5 * 24));
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();
    advance(&server, SignedDuration::from_hours(24));
    let err = server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated));
}

async fn stored(server: &TestServer, cookie: &SecretToken) -> SessionRecord {
    let hash = server.key.hash(TokenKind::Session, cookie.expose_secret());
    server
        .store
        .find_live_session(&hash, server.clock.now())
        .await
        .unwrap()
}

#[tokio::test]
async fn idle_extension_is_capped_at_absolute() {
    let (server, _, cookie) = signed_in().await;
    let created = server.clock.now();

    // Day 6, 12, 18, 24, 28: a day-28 touch would slide idle to day 35.
    for days in [6, 6, 6, 6, 4] {
        advance(&server, SignedDuration::from_hours(days * 24));
        server
            .sessions
            .resolve(cookie.expose_secret())
            .await
            .unwrap();
    }

    let record = stored(&server, &cookie).await;
    assert_eq!(
        record.absolute_expires_at,
        created + server.config.session_absolute
    );
    assert_eq!(record.idle_expires_at, record.absolute_expires_at);
}

#[tokio::test]
async fn touch_is_throttled_to_once_per_minute() {
    let (server, _, cookie) = signed_in().await;
    let created = server.clock.now();

    advance(&server, SignedDuration::from_secs(30));
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();
    let record = stored(&server, &cookie).await;
    assert_eq!(record.last_seen_at, created);
    assert_eq!(record.idle_expires_at, created + server.config.session_idle);

    advance(&server, SignedDuration::from_secs(31));
    let touched_at = server.clock.now();
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();
    let record = stored(&server, &cookie).await;
    assert_eq!(record.last_seen_at, touched_at);
    assert_eq!(
        record.idle_expires_at,
        touched_at + server.config.session_idle
    );
}

#[tokio::test]
async fn logout_invalidates_and_audits() {
    let (server, alice, cookie) = signed_in().await;

    server
        .sessions
        .logout(cookie.expose_secret())
        .await
        .unwrap();

    let err = server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated));
    let events = audit(&server).await;
    let logout = events
        .iter()
        .find(|event| event.action == "session.logout")
        .expect("logout audited");
    assert_eq!(
        logout.actor,
        AuditActor::Member {
            member: alice,
            agent: None
        }
    );
    // The entry names the member only: nothing identifies the session.
    assert_eq!(
        (logout.entity_type.as_str(), logout.entity_id.clone()),
        ("member", alice.to_string())
    );
    assert!(logout.changes.is_empty());
}

#[tokio::test]
async fn logout_of_unknown_session_succeeds_without_audit() {
    let (server, _, _) = signed_in().await;

    server.sessions.logout("not-a-session").await.unwrap();

    let events = audit(&server).await;
    assert!(events.iter().all(|e| e.action != "session.logout"));
}

#[tokio::test]
async fn touch_is_not_audited() {
    let (server, _, cookie) = signed_in().await;
    let before = audit(&server).await.len();

    advance(&server, SignedDuration::from_hours(24));
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();

    let after = audit(&server).await.len();
    assert_eq!(before, after);
}

#[tokio::test]
async fn resolve_uses_current_role_and_refuses_disabled_member() {
    let (server, alice, cookie) = signed_in().await;
    let admin = Actor {
        member: alice,
        role: Role::Admin,
        via: Via::Session,
    };
    let link = server
        .identity
        .create_invite(&admin, Role::Admin, None)
        .await
        .unwrap();
    let mut bob_auth = server.authenticator();
    let bob = bob_auth.enroll(&link, "bob", "Bob").await;
    let (_, bob_cookie) = bob_auth.login_with_session("bob").await;

    assert_eq!(
        server
            .sessions
            .resolve(bob_cookie.expose_secret())
            .await
            .unwrap()
            .role,
        Role::Admin
    );
    server
        .identity
        .set_role(&admin, bob, Role::Member)
        .await
        .unwrap();
    assert_eq!(
        server
            .sessions
            .resolve(bob_cookie.expose_secret())
            .await
            .unwrap()
            .role,
        Role::Member
    );

    server.identity.disable_member(&admin, bob).await.unwrap();
    let err = server
        .sessions
        .resolve(bob_cookie.expose_secret())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated));
    server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();
}

#[tokio::test]
async fn csrf_bound_to_session() {
    let (server, _, cookie_a) = signed_in().await;
    let cookie_b = server
        .sessions
        .create(
            server
                .sessions
                .resolve(cookie_a.expose_secret())
                .await
                .unwrap()
                .member,
        )
        .await
        .unwrap();

    let token_a = server.sessions.csrf_for(cookie_a.expose_secret());
    server
        .sessions
        .check_csrf(cookie_a.expose_secret(), &token_a)
        .unwrap();
    let err = server
        .sessions
        .check_csrf(cookie_b.expose_secret(), &token_a)
        .unwrap_err();
    assert!(matches!(err, AuthError::CsrfInvalid));
    let err = server
        .sessions
        .check_csrf(cookie_a.expose_secret(), "")
        .unwrap_err();
    assert!(matches!(err, AuthError::CsrfInvalid));
}

#[tokio::test]
async fn cookie_attributes() {
    let (_, _, cookie) = signed_in().await;

    let header = session_cookie(&cookie, SignedDuration::from_hours(30 * 24));

    let text = header.to_str().unwrap();
    assert!(text.starts_with(&format!("coffer_session={}", cookie.expose_secret())));
    for attribute in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=2592000",
    ] {
        assert!(text.contains(attribute), "the cookie lacks {attribute}");
    }
    assert!(header.is_sensitive());
}

#[tokio::test]
async fn clear_cookie_expires_immediately_with_same_attributes() {
    let header = coffer_server::auth::clear_session_cookie();

    let text = header.to_str().unwrap();
    for attribute in [
        "coffer_session=;",
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=0",
    ] {
        assert!(text.contains(attribute), "the cookie lacks {attribute}");
    }
}

#[tokio::test]
async fn activity_that_cannot_be_recorded_does_not_fail_the_request() {
    let (server, alice, cookie) = signed_in().await;
    advance(&server, SignedDuration::from_hours(1));
    // Another writer holds the lock past the busy timeout.
    let _blocker = server.store.begin().await.unwrap();

    let actor = server
        .sessions
        .resolve(cookie.expose_secret())
        .await
        .unwrap();

    assert_eq!(actor.member, alice);
}

#[tokio::test]
async fn session_ended_while_its_activity_is_recorded_is_refused() {
    let (server, _, cookie) = signed_in().await;
    advance(&server, SignedDuration::from_hours(1));
    let hash = server.key.hash(TokenKind::Session, cookie.expose_secret());
    let session = server
        .store
        .find_live_session(&hash, server.clock.now())
        .await
        .unwrap();
    // Hold the write lock so the request, once it has found the session
    // live, waits right where it records activity; end the session then.
    let mut blocker = server.store.begin().await.unwrap();
    let sessions = std::sync::Arc::clone(&server.sessions);
    let value = cookie.expose_secret().to_owned();
    let recording = event_signal("recording session activity", "session", session.id);
    let request = tokio::spawn(async move { sessions.resolve(&value).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), recording.notified())
        .await
        .unwrap();
    blocker.delete_session(session.id).await.unwrap();
    blocker.commit().await.unwrap();

    let err = request.await.unwrap().unwrap_err();
    assert!(matches!(err, AuthError::Unauthenticated), "{err:?}");
}
