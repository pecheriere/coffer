//! Bootstrap and invites: creating, consuming, expiring and revoking them.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{audit, display, label, session, username, with_admin, with_bob};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::{Role, Scope};
use coffer_domain::ids::MemberId;
use coffer_domain::policy::{Action, Denied, DenyReason};
use coffer_server::auth::AuthError;
use coffer_server::testkit::{TestServer, link_token};
use coffer_store::{Invite, StoreError};

#[tokio::test]
async fn bootstrap_creates_first_admin_then_disappears() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    assert_eq!(link.host_str(), Some("coffer.test"));
    assert!(
        link.path().starts_with("/setup/"),
        "the link must point at the setup path"
    );

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
    assert!(
        link.path().starts_with("/invite/"),
        "the link must point at the invite path"
    );

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
        assert!(
            !changes.contains(&token),
            "the audit changes must not hold the token"
        );
        assert!(
            !changes.contains("hash"),
            "the audit changes must not hold a hash"
        );
        assert!(
            !changes.contains("credential"),
            "the audit changes must not hold a credential"
        );
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
        "one redemption must report the invite as used"
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
