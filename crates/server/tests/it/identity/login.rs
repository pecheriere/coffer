//! Login ceremonies: failures, limits, user verification and signature
//! counters.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{display, label, session, username, with_admin, with_bob};
use coffer_domain::identity::Role;
use coffer_domain::time::Clock;
use coffer_server::auth::AuthError;
use coffer_server::testkit::link_token;
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_rs::prelude::RequestChallengeResponse;

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
