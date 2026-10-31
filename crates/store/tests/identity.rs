#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]
use coffer_domain::identity::{DisplayName, Role, Scope, TokenName, Username};
use coffer_domain::ids::{InviteId, MemberId, SessionId};
use coffer_store::{
    Invite, InviteKind, Member, NewAgentToken, NewInvite, NewMember, NewPasskey, NewSession, Store,
    StoreError,
};
use jiff::Timestamp;
use tempfile::TempDir;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap()
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

fn new_member(username: &str, role: Role) -> NewMember {
    NewMember {
        id: MemberId::new(),
        username: Username::parse(username).unwrap(),
        display_name: DisplayName::parse(username).unwrap(),
        role,
        created_at: at(1_000),
    }
}

async fn add_member(store: &Store, username: &str, role: Role) -> MemberId {
    let mut tx = store.begin().await.unwrap();
    let member = tx.insert_member(new_member(username, role)).await.unwrap();
    tx.commit().await.unwrap();
    member.id
}

async fn add_invite(store: &Store, hash: [u8; 32], expires_at: Timestamp) -> InviteId {
    let mut tx = store.begin().await.unwrap();
    let invite = tx
        .insert_invite(NewInvite {
            token_hash: hash,
            kind: InviteKind::Bootstrap,
            created_by: None,
            created_at: at(0),
            expires_at,
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    invite.id
}

#[tokio::test]
async fn username_unique() {
    let (_dir, store) = open().await;
    add_member(&store, "alice", Role::Admin).await;
    let mut tx = store.begin().await.unwrap();
    let err = tx
        .insert_member(new_member("Alice", Role::Member))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict("username taken")));
}

#[tokio::test]
async fn members_roundtrip_and_lookup() {
    let (_dir, store) = open().await;
    let id = add_member(&store, "alice", Role::Member).await;
    let by_id = store.get_member(id).await.unwrap();
    let by_name = store
        .get_member_by_username(&Username::parse("alice").unwrap())
        .await
        .unwrap();
    assert_eq!(by_id, by_name);
    assert_eq!(by_id.role, Role::Member);
    assert_eq!(by_id.disabled_at, None);
    assert_eq!(store.list_members().await.unwrap(), vec![by_id]);
    assert!(matches!(
        store.get_member(MemberId::new()).await,
        Err(StoreError::NotFound)
    ));

    let mut tx = store.begin().await.unwrap();
    tx.set_member_role(id, Role::Admin).await.unwrap();
    tx.set_member_disabled(id, at(5_000)).await.unwrap();
    tx.commit().await.unwrap();
    let updated = store.get_member(id).await.unwrap();
    assert_eq!(updated.role, Role::Admin);
    assert_eq!(updated.disabled_at, Some(at(5_000)));
}

#[tokio::test]
async fn count_active_admins_ignores_disabled() {
    let (_dir, store) = open().await;
    add_member(&store, "alice", Role::Admin).await;
    let bob = add_member(&store, "bob", Role::Admin).await;
    add_member(&store, "carol", Role::Member).await;
    let mut tx = store.begin().await.unwrap();
    assert_eq!(tx.count_active_admins().await.unwrap(), 2);
    tx.set_member_disabled(bob, at(2_000)).await.unwrap();
    assert_eq!(tx.count_active_admins().await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert!(store.any_admin_exists().await.unwrap());
}

#[tokio::test]
async fn any_admin_exists_false_without_admins() {
    let (_dir, store) = open().await;
    add_member(&store, "carol", Role::Member).await;
    assert!(!store.any_admin_exists().await.unwrap());
}

#[tokio::test]
async fn passkey_lifecycle() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let mut tx = store.begin().await.unwrap();
    let key = tx
        .insert_passkey(NewPasskey {
            member: alice,
            label: "laptop".into(),
            credential_id: vec![1, 2, 3],
            credential_json: "{\"v\":1}".into(),
            created_at: at(1_000),
        })
        .await
        .unwrap();
    assert_eq!(tx.count_passkeys(alice).await.unwrap(), 1);
    tx.update_passkey_after_use(key.id, "{\"v\":2}".into(), at(2_000))
        .await
        .unwrap();
    assert_eq!(
        tx.get_passkey(alice, key.id).await.unwrap().credential_json,
        "{\"v\":2}"
    );
    assert!(matches!(
        tx.get_passkey(MemberId::new(), key.id).await,
        Err(StoreError::NotFound)
    ));
    tx.commit().await.unwrap();

    let listed = store.list_passkeys(alice).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].credential_json, "{\"v\":2}");
    assert_eq!(listed[0].last_used_at, Some(at(2_000)));
    assert_eq!(listed[0].credential_id, vec![1, 2, 3]);

    let mut tx = store.begin().await.unwrap();
    tx.delete_passkey(alice, key.id).await.unwrap();
    assert_eq!(tx.count_passkeys(alice).await.unwrap(), 0);
}

#[tokio::test]
async fn delete_passkey_of_other_member_is_not_found() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let bob = add_member(&store, "bob", Role::Member).await;
    let mut tx = store.begin().await.unwrap();
    let key = tx
        .insert_passkey(NewPasskey {
            member: alice,
            label: "laptop".into(),
            credential_id: vec![9],
            credential_json: "{}".into(),
            created_at: at(1_000),
        })
        .await
        .unwrap();
    assert!(matches!(
        tx.delete_passkey(bob, key.id).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(tx.count_passkeys(alice).await.unwrap(), 1);
}

#[tokio::test]
async fn invite_expires_at_boundary() {
    let (_dir, store) = open().await;
    let t = at(10_000);
    add_invite(&store, [1; 32], t).await;
    store.find_usable_invite(&[1; 32], at(9_999)).await.unwrap();
    assert!(matches!(
        store.find_usable_invite(&[1; 32], t).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.find_usable_invite(&[2; 32], at(0)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn invite_kinds_roundtrip() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let mut tx = store.begin().await.unwrap();
    let kinds = [
        InviteKind::NewMember {
            role: Role::Member,
            display_name: Some(DisplayName::parse("Bob").unwrap()),
        },
        InviteKind::NewMember {
            role: Role::Admin,
            display_name: None,
        },
        InviteKind::Reenroll { member: alice },
    ];
    for (i, kind) in kinds.iter().enumerate() {
        tx.insert_invite(NewInvite {
            token_hash: [u8::try_from(i).unwrap(); 32],
            kind: kind.clone(),
            created_by: Some(alice),
            created_at: at(0),
            expires_at: at(10_000),
        })
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    for (i, kind) in kinds.iter().enumerate() {
        let found = store
            .find_usable_invite(&[u8::try_from(i).unwrap(); 32], at(1))
            .await
            .unwrap();
        assert_eq!(&found.kind, kind);
        assert_eq!(found.created_by, Some(alice));
    }
    assert_eq!(store.list_pending_invites(at(1)).await.unwrap().len(), 3);
    assert_eq!(
        store.list_pending_invites(at(10_000)).await.unwrap(),
        Vec::new()
    );
}

#[tokio::test]
async fn consume_invite_twice_conflicts() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let invite = add_invite(&store, [1; 32], at(10_000)).await;
    let mut tx = store.begin().await.unwrap();
    tx.consume_invite(invite, alice, at(1)).await.unwrap();
    let err = tx.consume_invite(invite, alice, at(2)).await.unwrap_err();
    assert!(matches!(
        err,
        StoreError::Conflict("invite already used or expired")
    ));
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_usable_invite(&[1; 32], at(3)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn consume_invite_at_expiry_conflicts() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let invite = add_invite(&store, [1; 32], at(10_000)).await;
    let mut tx = store.begin().await.unwrap();
    assert!(matches!(
        tx.consume_invite(invite, alice, at(10_000)).await,
        Err(StoreError::Conflict(_))
    ));
}

#[tokio::test]
async fn revoked_invite_unusable() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let invite = add_invite(&store, [1; 32], at(10_000)).await;
    let mut tx = store.begin().await.unwrap();
    tx.revoke_invite(invite, at(5)).await.unwrap();
    assert!(matches!(
        tx.consume_invite(invite, alice, at(6)).await,
        Err(StoreError::Conflict(_))
    ));
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_usable_invite(&[1; 32], at(6)).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(store.list_pending_invites(at(6)).await.unwrap(), Vec::new());
}

async fn add_session(
    store: &Store,
    member: MemberId,
    hash: [u8; 32],
    idle: i64,
    absolute: i64,
) -> SessionId {
    let mut tx = store.begin().await.unwrap();
    let session = tx
        .insert_session(NewSession {
            token_hash: hash,
            member,
            created_at: at(0),
            idle_expires_at: at(idle),
            absolute_expires_at: at(absolute),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    session.id
}

#[tokio::test]
async fn session_not_found_after_absolute_expiry_even_if_touched() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let id = add_session(&store, alice, [7; 32], 1_000, 5_000).await;
    assert_eq!(
        store.find_live_session(&[7; 32], at(999)).await.unwrap().id,
        id
    );
    let mut tx = store.begin().await.unwrap();
    tx.touch_session(id, at(900), at(9_000)).await.unwrap();
    tx.commit().await.unwrap();
    let live = store.find_live_session(&[7; 32], at(4_999)).await.unwrap();
    assert_eq!(live.member, alice);
    assert_eq!(live.last_seen_at, at(900));
    assert!(matches!(
        store.find_live_session(&[7; 32], at(5_000)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn session_not_found_after_idle_expiry() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    add_session(&store, alice, [7; 32], 1_000, 5_000).await;
    assert!(matches!(
        store.find_live_session(&[7; 32], at(1_000)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn deleting_sessions() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let bob = add_member(&store, "bob", Role::Member).await;
    let a1 = add_session(&store, alice, [1; 32], 1_000, 5_000).await;
    add_session(&store, alice, [2; 32], 1_000, 5_000).await;
    add_session(&store, bob, [3; 32], 1_000, 5_000).await;
    let mut tx = store.begin().await.unwrap();
    tx.delete_session(a1).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_live_session(&[1; 32], at(1)).await,
        Err(StoreError::NotFound)
    ));
    store.find_live_session(&[2; 32], at(1)).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    tx.delete_sessions_for_member(alice).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_live_session(&[2; 32], at(1)).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        store
            .find_live_session(&[3; 32], at(1))
            .await
            .unwrap()
            .member,
        bob
    );
}

async fn add_token(
    store: &Store,
    member: MemberId,
    hash: [u8; 32],
    expires_at: Option<Timestamp>,
) -> coffer_domain::ids::AgentTokenId {
    let mut tx = store.begin().await.unwrap();
    let token = tx
        .insert_agent_token(NewAgentToken {
            member,
            name: TokenName::parse("claude").unwrap(),
            scope: Scope::Write,
            token_hash: hash,
            prefix: "cof_abcd".into(),
            created_at: at(0),
            expires_at,
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    token.id
}

#[tokio::test]
async fn agent_token_roundtrip_and_touch() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let id = add_token(&store, alice, [1; 32], None).await;
    let mut tx = store.begin().await.unwrap();
    tx.touch_agent_token(id, at(500)).await.unwrap();
    tx.commit().await.unwrap();
    let found = store
        .find_live_agent_token(&[1; 32], at(600))
        .await
        .unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.scope, Scope::Write);
    assert_eq!(found.name.as_str(), "claude");
    assert_eq!(found.prefix, "cof_abcd");
    assert_eq!(found.last_used_at, Some(at(500)));
}

#[tokio::test]
async fn revoked_token_not_found() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let id = add_token(&store, alice, [1; 32], None).await;
    let mut tx = store.begin().await.unwrap();
    tx.revoke_agent_token(id, at(10)).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_live_agent_token(&[1; 32], at(11)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn expired_token_not_found() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    add_token(&store, alice, [1; 32], Some(at(100))).await;
    store.find_live_agent_token(&[1; 32], at(99)).await.unwrap();
    assert!(matches!(
        store.find_live_agent_token(&[1; 32], at(100)).await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn revoke_tokens_for_member_leaves_others() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let bob = add_member(&store, "bob", Role::Member).await;
    add_token(&store, alice, [1; 32], None).await;
    add_token(&store, alice, [2; 32], None).await;
    add_token(&store, bob, [3; 32], None).await;
    let mut tx = store.begin().await.unwrap();
    tx.revoke_tokens_for_member(alice, at(10)).await.unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.find_live_agent_token(&[1; 32], at(11)).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.find_live_agent_token(&[2; 32], at(11)).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        store
            .find_live_agent_token(&[3; 32], at(11))
            .await
            .unwrap()
            .member,
        bob
    );
}

#[tokio::test]
async fn list_agent_tokens_by_member() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let bob = add_member(&store, "bob", Role::Member).await;
    add_token(&store, alice, [1; 32], None).await;
    add_token(&store, bob, [2; 32], None).await;
    let only_alice = store.list_agent_tokens(Some(alice)).await.unwrap();
    assert_eq!(only_alice.len(), 1);
    assert_eq!(only_alice[0].member, alice);
    assert_eq!(store.list_agent_tokens(None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn get_agent_token_returns_revoked_and_reports_unknown() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let id = add_token(&store, alice, [1; 32], None).await;
    let mut tx = store.begin().await.unwrap();
    tx.revoke_agent_token(id, at(5)).await.unwrap();
    tx.commit().await.unwrap();

    let token = store.get_agent_token(id).await.unwrap();

    assert_eq!(token.id, id);
    assert_eq!(token.member, alice);
    assert_eq!(token.revoked_at, Some(at(5)));
    assert!(matches!(
        store
            .get_agent_token(coffer_domain::ids::AgentTokenId::new())
            .await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn concurrent_writers_do_not_fail_busy() {
    let (_dir, store) = open().await;
    let mut handles = Vec::new();
    for name in ["alice", "bob"] {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let mut tx = store.begin().await?;
            tx.count_active_admins().await?;
            tx.insert_member(new_member(name, Role::Admin)).await?;
            tx.commit().await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    assert_eq!(store.list_members().await.unwrap().len(), 2);
}

#[tokio::test]
async fn transaction_reads_see_its_own_writes() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    let new = new_member("alice", Role::Admin);
    let id = new.id;
    tx.insert_member(new).await.unwrap();
    assert_eq!(tx.get_member(id).await.unwrap().id, id);
    assert!(matches!(
        tx.get_member(MemberId::new()).await,
        Err(StoreError::NotFound)
    ));
    let invite = tx
        .insert_invite(NewInvite {
            token_hash: [5; 32],
            kind: InviteKind::Bootstrap,
            created_by: None,
            created_at: at(1_000),
            expires_at: at(2_000),
        })
        .await
        .unwrap();
    assert_eq!(
        tx.find_usable_invite(&[5; 32], at(1_500)).await.unwrap().id,
        invite.id
    );
    assert_eq!(tx.list_pending_invites(at(1_500)).await.unwrap().len(), 1);
    tx.consume_invite(invite.id, id, at(1_500)).await.unwrap();
    assert!(matches!(
        tx.find_usable_invite(&[5; 32], at(1_500)).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        tx.list_pending_invites(at(1_500)).await.unwrap(),
        Vec::<Invite>::new()
    );
    drop(tx);
    assert_eq!(store.list_members().await.unwrap(), Vec::<Member>::new());
}

#[tokio::test]
async fn touching_a_revoked_token_is_not_found() {
    let (_dir, store) = open().await;
    let alice = add_member(&store, "alice", Role::Admin).await;
    let id = add_token(&store, alice, [1; 32], None).await;
    let mut tx = store.begin().await.unwrap();
    tx.revoke_agent_token(id, at(10)).await.unwrap();

    assert!(matches!(
        tx.touch_agent_token(id, at(11)).await,
        Err(StoreError::NotFound)
    ));
}
