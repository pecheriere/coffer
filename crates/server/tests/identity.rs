//! Identity service: bootstrap, invites, enrollment, re-enroll, login and
//! member management, driven with software passkeys.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_domain::audit::{AuditActor, AuditChanges, AuditValue};
use coffer_domain::identity::{Actor, DisplayName, Role, Scope, TokenName, Username, Via};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::policy::{Action, Denied, DenyReason};
use coffer_domain::time::Clock;
use coffer_server::auth::AuthError;
use coffer_server::testkit::{SoftAuthenticator, TestServer, link_token};
use coffer_store::{
    AuditEvent, AuditFilter, Invite, NewAgentToken, NewSession, PageRequest, StoreError,
};
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_rs::prelude::RequestChallengeResponse;

fn session(member: MemberId, role: Role) -> Actor {
    Actor {
        member,
        role,
        via: Via::Session,
    }
}

fn username(name: &str) -> Username {
    Username::parse(name).unwrap()
}

fn display(name: &str) -> DisplayName {
    DisplayName::parse(name).unwrap()
}

fn label(name: &str) -> TokenName {
    TokenName::parse(name).unwrap()
}

/// A server whose first admin, "alice", has enrolled with the returned
/// authenticator.
async fn with_admin() -> (TestServer, SoftAuthenticator, MemberId) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut auth = server.authenticator();
    let alice = auth.enroll(&link, "alice", "Alice").await;
    (server, auth, alice)
}

/// Alice invites "bob" as a plain member; bob enrolls with the returned
/// authenticator.
async fn with_bob(server: &TestServer, alice: MemberId) -> (SoftAuthenticator, MemberId) {
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let mut auth = server.authenticator();
    let bob = auth.enroll(&link, "bob", "Bob").await;
    (auth, bob)
}

async fn audit(server: &TestServer) -> Vec<AuditEvent> {
    let mut events = server
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
        .items;
    events.reverse();
    events
}

#[tokio::test]
async fn bootstrap_creates_first_admin_then_disappears() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    assert_eq!(link.host_str(), Some("coffer.test"));
    assert!(link.path().starts_with("/setup/"), "{link}");

    let alice = server.authenticator().enroll(&link, "alice", "Alice").await;
    let member = server.store.get_member(alice).await.unwrap();
    assert_eq!(member.role, Role::Admin);
    assert_eq!(member.username, username("alice"));

    assert!(server.identity.bootstrap_link().await.unwrap().is_none());
    let events = audit(&server).await;
    assert_eq!(
        events[0].actor,
        AuditActor::System {
            reason: "system:bootstrap".into()
        }
    );
    assert_eq!(events[0].action, "invite.create");
}

#[tokio::test]
async fn new_bootstrap_link_revokes_previous() {
    let server = TestServer::new().await;
    let first = server.identity.bootstrap_link().await.unwrap().unwrap();
    let second = server.identity.bootstrap_link().await.unwrap().unwrap();

    let err = server
        .identity
        .begin_enrollment(
            &link_token(&first),
            Some(username("alice")),
            Some(display("Alice")),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InviteInvalid), "{err:?}");

    server
        .authenticator()
        .enroll(&second, "alice", "Alice")
        .await;
}

#[tokio::test]
async fn invite_creates_member_with_invite_role() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    assert!(link.path().starts_with("/invite/"), "{link}");

    let bob = server.authenticator().enroll(&link, "bob", "Bob").await;
    assert_eq!(
        server.store.get_member(bob).await.unwrap().role,
        Role::Member
    );

    let events = audit(&server).await;
    let invite_create = events
        .iter()
        .rfind(|e| e.action == "invite.create")
        .unwrap();
    assert_eq!(
        invite_create.actor,
        AuditActor::Member {
            member: alice,
            agent: None
        }
    );
    // Each invite property is its own field, set by the creation.
    let fields: Vec<_> = invite_create.changes.iter().collect();
    assert_eq!(
        fields.iter().map(|(field, _)| *field).collect::<Vec<_>>(),
        ["display_name", "expires_at", "kind", "role"]
    );
    assert!(
        fields
            .iter()
            .all(|(_, change)| change.before == AuditValue::Null)
    );
    let after = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, change)| change.after.clone())
            .unwrap()
    };
    assert_eq!(after("kind"), AuditValue::from("new_member"));
    assert_eq!(after("role"), AuditValue::from("member"));
    assert_eq!(after("display_name"), AuditValue::Null);
    assert!(matches!(after("expires_at"), AuditValue::Text(_)));
    let member_create = events
        .iter()
        .rfind(|e| e.action == "member.create")
        .unwrap();
    assert_eq!(member_create.entity_id, bob.to_string());
    assert_eq!(
        member_create.actor,
        AuditActor::Member {
            member: bob,
            agent: None
        }
    );
    assert!(events.iter().any(|e| e.action == "passkey.add"
        && e.actor
            == AuditActor::Member {
                member: bob,
                agent: None
            }));

    // Neither the invite token nor its hash ends up in the audit log.
    let token = link_token(&link);
    for event in &events {
        let changes = serde_json::to_string(&event.changes).unwrap();
        assert!(!changes.contains(&token), "{changes}");
        assert!(!changes.contains("hash"), "{changes}");
        assert!(!changes.contains("credential"), "{changes}");
    }
}

#[tokio::test]
async fn invite_display_name_is_the_fallback() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(
            &session(alice, Role::Admin),
            Role::Admin,
            Some(display("Bobby")),
        )
        .await
        .unwrap();
    let (ceremony, options) = server
        .identity
        .begin_enrollment(&link_token(&link), Some(username("bob")), None)
        .await
        .unwrap();
    let mut auth = server.authenticator();
    let credential = auth.register(options).unwrap();
    let bob = server
        .identity
        .finish_enrollment(ceremony, label("laptop"), credential)
        .await
        .unwrap();
    let member = server.store.get_member(bob).await.unwrap();
    assert_eq!(member.display_name, display("Bobby"));
    assert_eq!(member.role, Role::Admin);
}

#[tokio::test]
async fn new_member_needs_a_free_username() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let token = link_token(&link);

    let duplicate = server
        .identity
        .begin_enrollment(&token, Some(username("alice")), Some(display("Other")))
        .await
        .unwrap_err();
    assert!(
        matches!(duplicate, AuthError::UsernameTaken),
        "{duplicate:?}"
    );

    let missing = server
        .identity
        .begin_enrollment(&token, None, Some(display("Bob")))
        .await
        .unwrap_err();
    assert!(matches!(missing, AuthError::Invalid(_)), "{missing:?}");

    let no_name = server
        .identity
        .begin_enrollment(&token, Some(username("bob")), None)
        .await
        .unwrap_err();
    assert!(matches!(no_name, AuthError::Invalid(_)), "{no_name:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invite_consumed_once_under_race() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let token = link_token(&link);

    let mut first = server.authenticator();
    let mut second = server.authenticator();
    let (c1, o1) = server
        .identity
        .begin_enrollment(&token, Some(username("bob")), Some(display("Bob")))
        .await
        .unwrap();
    let (c2, o2) = server
        .identity
        .begin_enrollment(&token, Some(username("carol")), Some(display("Carol")))
        .await
        .unwrap();
    let r1 = first.register(o1).unwrap();
    let r2 = second.register(o2).unwrap();

    let (a, b) = tokio::join!(
        server.identity.finish_enrollment(c1, label("one"), r1),
        server.identity.finish_enrollment(c2, label("two"), r2),
    );
    let outcomes = [a, b];
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .any(|r| matches!(r, Err(AuthError::InviteUsed))),
        "{outcomes:?}"
    );
    assert_eq!(server.store.list_members().await.unwrap().len(), 2);
}

#[tokio::test]
async fn expired_invite_rejected() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    server.clock.advance(server.config.invite_ttl);
    let err = server
        .identity
        .begin_enrollment(
            &link_token(&link),
            Some(username("bob")),
            Some(display("Bob")),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InviteInvalid), "{err:?}");
}

#[tokio::test]
async fn revoked_invite_rejected_and_no_longer_pending() {
    let (server, _, alice) = with_admin().await;
    let admin = session(alice, Role::Admin);
    let link = server
        .identity
        .create_invite(&admin, Role::Member, None)
        .await
        .unwrap();
    let pending = server.identity.list_pending_invites(&admin).await.unwrap();
    assert_eq!(pending.len(), 1);

    server
        .identity
        .revoke_invite(&admin, pending[0].id)
        .await
        .unwrap();
    assert_eq!(
        server.identity.list_pending_invites(&admin).await.unwrap(),
        Vec::<Invite>::new()
    );
    let err = server
        .identity
        .begin_enrollment(
            &link_token(&link),
            Some(username("bob")),
            Some(display("Bob")),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InviteInvalid), "{err:?}");
    assert!(
        audit(&server)
            .await
            .iter()
            .any(|e| e.action == "invite.revoke" && e.entity_id == pending[0].id.to_string())
    );
}

#[tokio::test]
async fn member_cannot_create_invite() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let bob_session = session(bob, Role::Member);
    let needs_admin = |action| {
        AuthError::Denied(Denied {
            action,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin,
            },
        })
        .to_string()
    };

    let err = server
        .identity
        .create_invite(&bob_session, Role::Member, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AuthError::Denied(d) if d.action == Action::IssueEnrollmentLink),
        "{err:?}"
    );
    assert_eq!(err.to_string(), needs_admin(Action::IssueEnrollmentLink));

    let err = server
        .identity
        .list_pending_invites(&bob_session)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    let err = server
        .identity
        .create_reenroll(&bob_session, alice)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AuthError::Denied(d) if d.action == Action::IssueEnrollmentLink),
        "{err:?}"
    );
    let err = server
        .identity
        .set_role(&bob_session, bob, Role::Admin)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    let err = server
        .identity
        .disable_member(&bob_session, alice)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    assert_eq!(
        server.store.get_member(bob).await.unwrap().role,
        Role::Member
    );
}

#[tokio::test]
async fn reenroll_adds_passkey_to_existing_member() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let link = server
        .identity
        .create_reenroll(&session(alice, Role::Admin), bob)
        .await
        .unwrap();
    assert!(link.path().starts_with("/invite/"), "{link}");

    let mut replacement = server.authenticator();
    let enrolled = replacement.enroll(&link, "ignored", "Ignored").await;
    assert_eq!(enrolled, bob);
    assert_eq!(server.store.list_passkeys(bob).await.unwrap().len(), 2);
    assert_eq!(server.store.list_members().await.unwrap().len(), 2);

    assert_eq!(replacement.login("bob").await, bob);
    let login = audit(&server)
        .await
        .into_iter()
        .rfind(|e| e.action == "session.login")
        .unwrap();
    assert_eq!(
        login.actor,
        AuditActor::Member {
            member: bob,
            agent: None
        }
    );
    let passkeys = server.store.list_passkeys(bob).await.unwrap();
    assert!(passkeys.iter().any(|p| p.last_used_at.is_some()));
}

#[tokio::test]
async fn cannot_remove_last_passkey() {
    let (server, _, alice) = with_admin().await;
    let actor = session(alice, Role::Admin);
    let only = server.store.list_passkeys(alice).await.unwrap()[0].id;
    let err = server
        .identity
        .remove_passkey(&actor, only)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::LastPasskey), "{err:?}");

    // Credentials the member already has are excluded from registration.
    let (_, options) = server.identity.begin_add_passkey(&actor).await.unwrap();
    let options = serde_json::to_value(options).unwrap();
    let stored = server.store.list_passkeys(alice).await.unwrap()[0]
        .credential_id
        .clone();
    assert_eq!(
        options["publicKey"]["excludeCredentials"],
        serde_json::json!([{
            "type": "public-key",
            "id": URL_SAFE_NO_PAD.encode(stored),
        }]),
    );

    let mut second = server.authenticator();
    let (ceremony, options) = server.identity.begin_add_passkey(&actor).await.unwrap();
    let credential = second.register(options).unwrap();
    let added = server
        .identity
        .finish_add_passkey(&actor, ceremony, label("phone"), credential)
        .await
        .unwrap();
    assert_eq!(server.store.list_passkeys(alice).await.unwrap().len(), 2);

    server.identity.remove_passkey(&actor, only).await.unwrap();
    let left = server.store.list_passkeys(alice).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, added);
    assert_eq!(left[0].label, "phone");
    assert_eq!(second.login("alice").await, alice);

    let events = audit(&server).await;
    let removed = events
        .iter()
        .find(|e| e.action == "passkey.remove")
        .unwrap();
    assert_eq!(removed.entity_id, only.to_string());
    assert_eq!(
        removed.changes,
        AuditChanges::new().unset("label", "soft passkey")
    );

    // Another member's passkey cannot be removed, and agents cannot manage
    // passkeys at all.
    let (_, bob) = with_bob(&server, alice).await;
    let bobs = server.store.list_passkeys(bob).await.unwrap()[0].id;
    let err = server
        .identity
        .remove_passkey(&actor, bobs)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::NotFound), "{err:?}");
    let agent = Actor {
        member: alice,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Admin,
        },
    };
    let err = server.identity.begin_add_passkey(&agent).await.unwrap_err();
    assert!(
        matches!(&err, AuthError::Denied(d) if d.reason == DenyReason::SessionRequired),
        "{err:?}"
    );
}

#[tokio::test]
async fn cannot_disable_last_admin() {
    let (server, _, alice) = with_admin().await;
    let admin = session(alice, Role::Admin);
    let err = server
        .identity
        .disable_member(&admin, alice)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::LastAdmin), "{err:?}");
    assert!(
        server
            .store
            .get_member(alice)
            .await
            .unwrap()
            .disabled_at
            .is_none()
    );

    // With a second admin, the first may step down.
    let (_, bob) = with_bob(&server, alice).await;
    server
        .identity
        .set_role(&admin, bob, Role::Admin)
        .await
        .unwrap();
    server.identity.disable_member(&admin, alice).await.unwrap();
    assert!(
        server
            .store
            .get_member(alice)
            .await
            .unwrap()
            .disabled_at
            .is_some()
    );
}

#[tokio::test]
async fn disabling_member_ends_sessions_tokens_and_logins() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let now = server.clock.now();
    let session_hash = [7_u8; 32];
    let token_hash = [9_u8; 32];
    let mut tx = server.store.begin().await.unwrap();
    tx.insert_session(NewSession {
        token_hash: session_hash,
        member: bob,
        created_at: now,
        idle_expires_at: now + server.config.session_idle,
        absolute_expires_at: now + server.config.session_absolute,
    })
    .await
    .unwrap();
    tx.insert_agent_token(NewAgentToken {
        member: bob,
        name: label("agent"),
        scope: Scope::Read,
        token_hash,
        prefix: "coffer_abcde".into(),
        created_at: now,
        expires_at: None,
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();

    server
        .identity
        .disable_member(&session(alice, Role::Admin), bob)
        .await
        .unwrap();

    assert!(matches!(
        server.store.find_live_session(&session_hash, now).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        server.store.find_live_agent_token(&token_hash, now).await,
        Err(StoreError::NotFound)
    ));
    let unknown = server
        .identity
        .begin_login(&username("nobody"))
        .await
        .unwrap_err();
    let disabled = server
        .identity
        .begin_login(&username("bob"))
        .await
        .unwrap_err();
    assert!(matches!(disabled, AuthError::LoginFailed), "{disabled:?}");
    assert_eq!(disabled.to_string(), unknown.to_string());

    // A disabled member cannot be re-enrolled either.
    let err = server
        .identity
        .create_reenroll(&session(alice, Role::Admin), bob)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::MemberDisabled), "{err:?}");
    let disabled = audit(&server)
        .await
        .into_iter()
        .find(|e| e.action == "member.disable" && e.entity_id == bob.to_string())
        .unwrap();
    assert_eq!(
        disabled.changes,
        AuditChanges::new().set("disabled_at", now)
    );
}

#[tokio::test]
async fn admin_cannot_demote_self_if_last() {
    let (server, _, alice) = with_admin().await;
    let admin = session(alice, Role::Admin);
    let err = server
        .identity
        .set_role(&admin, alice, Role::Member)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::LastAdmin), "{err:?}");
    assert_eq!(
        server.store.get_member(alice).await.unwrap().role,
        Role::Admin
    );

    let (_, bob) = with_bob(&server, alice).await;
    server
        .identity
        .set_role(&admin, bob, Role::Admin)
        .await
        .unwrap();
    server
        .identity
        .set_role(&admin, alice, Role::Member)
        .await
        .unwrap();
    assert_eq!(
        server.store.get_member(alice).await.unwrap().role,
        Role::Member
    );
    let role_changes: Vec<_> = audit(&server)
        .await
        .into_iter()
        .filter(|e| e.action == "member.role")
        .map(|e| e.changes)
        .collect();
    assert_eq!(
        role_changes,
        [
            AuditChanges::new().change("role", "member", "admin"),
            AuditChanges::new().change("role", "admin", "member"),
        ]
    );
}

#[tokio::test]
async fn login_failures_share_one_message() {
    let (server, mut auth, alice) = with_admin().await;
    assert_eq!(auth.login("alice").await, alice);

    let unknown = server
        .identity
        .begin_login(&username("nobody"))
        .await
        .unwrap_err();
    assert!(matches!(unknown, AuthError::LoginFailed), "{unknown:?}");

    // An assertion over another ceremony's challenge does not verify.
    let (_, signed_options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let (ceremony, _) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let credential = auth.sign(signed_options);
    let bad = server
        .identity
        .finish_login(ceremony, credential)
        .await
        .unwrap_err();
    assert!(matches!(bad, AuthError::LoginFailed), "{bad:?}");
    assert_eq!(unknown.to_string(), bad.to_string());
}

#[tokio::test]
async fn login_ceremony_is_single_use_and_expires() {
    let (server, mut auth, _) = with_admin().await;
    let (ceremony, options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let credential = auth.sign(options);
    server
        .identity
        .finish_login(ceremony.clone(), credential.clone())
        .await
        .unwrap();
    let replay = server
        .identity
        .finish_login(ceremony, credential)
        .await
        .unwrap_err();
    assert!(matches!(replay, AuthError::CeremonyExpired), "{replay:?}");

    let (ceremony, options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let credential = auth.sign(options);
    server.clock.advance(jiff::SignedDuration::from_secs(120));
    let late = server
        .identity
        .finish_login(ceremony, credential)
        .await
        .unwrap_err();
    assert!(matches!(late, AuthError::CeremonyExpired), "{late:?}");
}

#[tokio::test]
async fn login_requires_user_verification() {
    let (server, mut auth, _) = with_admin().await;

    // Registration asks for user verification, so an authenticator that
    // cannot verify the user declines to register.
    let link = server.identity.bootstrap_link().await.unwrap();
    assert!(link.is_none());
    let admin_link = server
        .identity
        .create_invite(
            &session(
                server
                    .store
                    .get_member_by_username(&username("alice"))
                    .await
                    .unwrap()
                    .id,
                Role::Admin,
            ),
            Role::Member,
            None,
        )
        .await
        .unwrap();
    let (_, options) = server
        .identity
        .begin_enrollment(
            &link_token(&admin_link),
            Some(username("bob")),
            Some(display("Bob")),
        )
        .await
        .unwrap();
    assert!(matches!(
        server
            .authenticator_without_user_verification()
            .register(options),
        Err(WebauthnCError::NotSupported)
    ));

    // The server enforces it too: a validly signed assertion with the UV
    // flag clear is refused.
    let (ceremony, options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let mut options = serde_json::to_value(options).unwrap();
    assert_eq!(options["publicKey"]["userVerification"], "required");
    options["publicKey"]["userVerification"] = "discouraged".into();
    let options: RequestChallengeResponse = serde_json::from_value(options).unwrap();
    let credential = auth.sign(options);
    let flags = credential.response.authenticator_data.as_ref()[32];
    assert_eq!(flags & 0b101, 0b001, "UP set and UV clear");
    let err = server
        .identity
        .finish_login(ceremony, credential)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::LoginFailed), "{err:?}");
}

#[tokio::test]
async fn cli_admin_reenroll_only_for_admins() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;

    let err = server
        .identity
        .cli_admin_reenroll(&username("bob"))
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    let err = server
        .identity
        .cli_admin_reenroll(&username("nobody"))
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::NotFound), "{err:?}");
    assert_eq!(server.store.list_passkeys(bob).await.unwrap().len(), 1);

    let link = server
        .identity
        .cli_admin_reenroll(&username("alice"))
        .await
        .unwrap();
    let mut recovered = server.authenticator();
    assert_eq!(recovered.enroll(&link, "ignored", "Ignored").await, alice);
    assert_eq!(recovered.login("alice").await, alice);
    let events = audit(&server).await;
    let created = events
        .iter()
        .rfind(|e| e.action == "invite.create")
        .unwrap();
    assert_eq!(
        created.actor,
        AuditActor::System {
            reason: "system:cli".into()
        }
    );
}

#[tokio::test]
async fn ceremonies_are_capped_until_they_expire() {
    let (server, _, alice) = with_admin().await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let token = link_token(&link);
    let begin = || {
        server
            .identity
            .begin_enrollment(&token, Some(username("bob")), Some(display("Bob")))
    };
    for _ in 0..32 {
        begin().await.unwrap();
    }
    let err = begin().await.unwrap_err();
    assert!(matches!(err, AuthError::Busy), "{err:?}");

    server.clock.advance(jiff::SignedDuration::from_secs(120));
    begin().await.unwrap();
}

#[tokio::test]
async fn login_flood_does_not_block_enrollment() {
    let (server, _, alice) = with_admin().await;
    let (_, _) = with_bob(&server, alice).await;
    let link = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    for _ in 0..100 {
        for name in ["alice", "bob"] {
            server.identity.begin_login(&username(name)).await.unwrap();
        }
    }
    server.authenticator().enroll(&link, "carol", "Carol").await;
}

#[tokio::test]
async fn login_ceremonies_capped_per_member() {
    let (server, mut auth, _) = with_admin().await;
    let alice = username("alice");
    let (oldest, oldest_options) = server.identity.begin_login(&alice).await.unwrap();
    let mut newer = Vec::new();
    for _ in 0..4 {
        newer.push(server.identity.begin_login(&alice).await.unwrap());
    }
    // The fifth pending sign-in evicted the first.
    let credential = auth.sign(oldest_options);
    let err = server
        .identity
        .finish_login(oldest, credential)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::CeremonyExpired), "{err:?}");
    for (ceremony, options) in newer {
        let credential = auth.sign(options);
        server
            .identity
            .finish_login(ceremony, credential)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn agents_cannot_issue_enrollment_links() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let agent = Actor {
        member: alice,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Admin,
        },
    };
    let session_required = |err: &AuthError| {
        matches!(err, AuthError::Denied(d)
            if d.action == Action::IssueEnrollmentLink
                && d.reason == DenyReason::SessionRequired)
    };
    let err = server
        .identity
        .create_invite(&agent, Role::Admin, None)
        .await
        .unwrap_err();
    assert!(session_required(&err), "{err:?}");
    let err = server
        .identity
        .create_reenroll(&agent, bob)
        .await
        .unwrap_err();
    assert!(session_required(&err), "{err:?}");
    let err = server
        .identity
        .create_reenroll(&agent, alice)
        .await
        .unwrap_err();
    assert!(session_required(&err), "{err:?}");

    // Admin agents keep the other member and invite administration.
    assert_eq!(
        server.identity.list_pending_invites(&agent).await.unwrap(),
        Vec::<Invite>::new()
    );
    server
        .identity
        .set_role(&agent, bob, Role::Admin)
        .await
        .unwrap();
}

fn stored_counter(record: &coffer_store::PasskeyRecord) -> u64 {
    let json: serde_json::Value = serde_json::from_str(&record.credential_json).unwrap();
    json["cred"]["counter"].as_u64().unwrap()
}

#[tokio::test]
async fn stored_counter_never_decreases() {
    let (server, mut auth, alice) = with_admin().await;
    let name = username("alice");
    let (first, first_options) = server.identity.begin_login(&name).await.unwrap();
    let (second, second_options) = server.identity.begin_login(&name).await.unwrap();
    let first_credential = auth.sign(first_options);
    let second_credential = auth.sign(second_options);

    // The later signature lands first; the earlier one must not roll the
    // stored counter back.
    server
        .identity
        .finish_login(second, second_credential)
        .await
        .unwrap();
    let after_second = stored_counter(&server.store.list_passkeys(alice).await.unwrap()[0]);
    server
        .identity
        .finish_login(first, first_credential)
        .await
        .unwrap();
    let after_first = stored_counter(&server.store.list_passkeys(alice).await.unwrap()[0]);
    assert!(after_second > 0);
    assert_eq!(after_first, after_second);
}

#[tokio::test]
async fn counter_regression_fails_login() {
    let (server, mut auth, alice) = with_admin().await;
    let record = server.store.list_passkeys(alice).await.unwrap().remove(0);
    let mut json: serde_json::Value = serde_json::from_str(&record.credential_json).unwrap();
    json["cred"]["counter"] = 1_000.into();
    let mut tx = server.store.begin().await.unwrap();
    tx.update_passkey_after_use(record.id, json.to_string(), server.clock.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (ceremony, options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let credential = auth.sign(options);
    let err = server
        .identity
        .finish_login(ceremony, credential)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::LoginFailed), "{err:?}");
    let stored = stored_counter(&server.store.list_passkeys(alice).await.unwrap()[0]);
    assert_eq!(stored, 1_000);
}

/// Alice promotes bob to admin and bob issues a new-member invite; returns
/// bob, his invite link and the invite's id.
async fn bob_admin_with_invite(
    server: &TestServer,
    alice: MemberId,
) -> (MemberId, url::Url, coffer_domain::ids::InviteId) {
    let (_, bob) = with_bob(server, alice).await;
    server
        .identity
        .set_role(&session(alice, Role::Admin), bob, Role::Admin)
        .await
        .unwrap();
    let link = server
        .identity
        .create_invite(&session(bob, Role::Admin), Role::Member, None)
        .await
        .unwrap();
    let invite = server
        .identity
        .list_pending_invites(&session(alice, Role::Admin))
        .await
        .unwrap()
        .into_iter()
        .find(|invite| invite.created_by == Some(bob))
        .unwrap()
        .id;
    (bob, link, invite)
}

/// The invite is no longer redeemable, and its revocation was audited with
/// alice as the actor; alice's own invite is untouched.
async fn assert_revoked_by_alice(
    server: &TestServer,
    alice: MemberId,
    link: &url::Url,
    invite: coffer_domain::ids::InviteId,
    alices_invite: &url::Url,
) {
    let err = server
        .identity
        .begin_enrollment(
            &link_token(link),
            Some(username("carol")),
            Some(display("Carol")),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InviteInvalid), "{err:?}");
    let pending = server
        .identity
        .list_pending_invites(&session(alice, Role::Admin))
        .await
        .unwrap();
    assert!(pending.iter().all(|candidate| candidate.id != invite));
    assert_eq!(pending.len(), 1, "alice's own invite stays pending");
    let revoke = audit(server)
        .await
        .into_iter()
        .find(|e| e.action == "invite.revoke" && e.entity_id == invite.to_string())
        .unwrap();
    assert_eq!(
        revoke.actor,
        AuditActor::Member {
            member: alice,
            agent: None
        }
    );
    server
        .authenticator()
        .enroll(alices_invite, "dave", "Dave")
        .await;
}

#[tokio::test]
async fn disabling_an_admin_revokes_their_pending_invites() {
    let (server, _, alice) = with_admin().await;
    let (bob, link, invite) = bob_admin_with_invite(&server, alice).await;
    let alices_invite = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();

    server
        .identity
        .disable_member(&session(alice, Role::Admin), bob)
        .await
        .unwrap();

    assert_revoked_by_alice(&server, alice, &link, invite, &alices_invite).await;
}

#[tokio::test]
async fn demoting_an_admin_revokes_their_pending_invites() {
    let (server, _, alice) = with_admin().await;
    let (bob, link, invite) = bob_admin_with_invite(&server, alice).await;
    let alices_invite = server
        .identity
        .create_invite(&session(alice, Role::Admin), Role::Member, None)
        .await
        .unwrap();

    server
        .identity
        .set_role(&session(alice, Role::Admin), bob, Role::Member)
        .await
        .unwrap();

    assert_revoked_by_alice(&server, alice, &link, invite, &alices_invite).await;
}

#[tokio::test]
async fn invite_from_a_former_admin_cannot_be_finished() {
    let (server, _, alice) = with_admin().await;
    let (bob, link, _) = bob_admin_with_invite(&server, alice).await;
    let (ceremony, options) = server
        .identity
        .begin_enrollment(
            &link_token(&link),
            Some(username("carol")),
            Some(display("Carol")),
        )
        .await
        .unwrap();
    let credential = server.authenticator().register(options).unwrap();

    // Demoted behind the service's back, so the invite itself stays pending:
    // the redemption must still check who issued it.
    let mut tx = server.store.begin().await.unwrap();
    tx.set_member_role(bob, Role::Member).await.unwrap();
    tx.commit().await.unwrap();

    let err = server
        .identity
        .finish_enrollment(ceremony, label("passkey"), credential)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::InviteInvalid), "{err:?}");
    assert!(
        server
            .store
            .get_member_by_username(&username("carol"))
            .await
            .is_err_and(|err| matches!(err, StoreError::NotFound))
    );
}

#[tokio::test]
async fn add_passkey_ceremony_is_bound_to_the_member_who_began_it() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let (ceremony, options) = server
        .identity
        .begin_add_passkey(&session(alice, Role::Admin))
        .await
        .unwrap();
    let credential = server.authenticator().register(options).unwrap();

    let err = server
        .identity
        .finish_add_passkey(
            &session(bob, Role::Member),
            ceremony,
            label("stolen"),
            credential,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, AuthError::CeremonyExpired), "{err:?}");
    assert_eq!(server.store.list_passkeys(alice).await.unwrap().len(), 1);
    assert_eq!(server.store.list_passkeys(bob).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_ceremony_of_one_kind_cannot_finish_another() {
    let (server, mut auth, alice) = with_admin().await;
    let admin = session(alice, Role::Admin);
    // A valid registration response, to present against the wrong ceremony.
    let (_, options) = server.identity.begin_add_passkey(&admin).await.unwrap();
    let registration = server.authenticator().register(options).unwrap();

    let (login, _) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let err = server
        .identity
        .finish_enrollment(login, label("passkey"), registration.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::CeremonyExpired), "{err:?}");

    let (login, _) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let err = server
        .identity
        .finish_add_passkey(&admin, login, label("passkey"), registration)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::CeremonyExpired), "{err:?}");

    let invite = server
        .identity
        .create_invite(&admin, Role::Member, None)
        .await
        .unwrap();
    let (enrollment, _) = server
        .identity
        .begin_enrollment(
            &link_token(&invite),
            Some(username("bob")),
            Some(display("Bob")),
        )
        .await
        .unwrap();
    let (_, login_options) = server
        .identity
        .begin_login(&username("alice"))
        .await
        .unwrap();
    let assertion = auth.sign(login_options);
    let err = server
        .identity
        .finish_login(enrollment, assertion)
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::CeremonyExpired), "{err:?}");

    assert_eq!(server.store.list_passkeys(alice).await.unwrap().len(), 1);
    assert_eq!(server.store.list_members().await.unwrap().len(), 1);
}
