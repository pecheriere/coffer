//! Passkeys and members: re-enrolling, last-passkey and last-admin rules,
//! disabling and demoting.

use super::{audit, label, session, username, with_admin, with_bob};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_domain::audit::{AuditActor, AuditChanges};
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::ids::AgentTokenId;
use coffer_domain::policy::{Action, DenyReason};
use coffer_domain::time::Clock;
use coffer_server::auth::AuthError;
use coffer_store::{Invite, NewAgentToken, NewSession, StoreError};

#[tokio::test]
async fn reenroll_adds_passkey_to_existing_member() {
    let (server, _, alice) = with_admin().await;
    let (_, bob) = with_bob(&server, alice).await;
    let link = server
        .identity
        .create_reenroll(&session(alice, Role::Admin), bob)
        .await
        .unwrap();
    assert!(
        link.path().starts_with("/invite/"),
        "the link must point at the invite path"
    );

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
