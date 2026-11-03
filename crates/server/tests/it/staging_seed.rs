//! `serve` refuses the staging seed, whose admin passkey is public, away from localhost.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use coffer_domain::identity::{Actor, DisplayName, Role, TokenName, Username, Via};
use coffer_domain::ids::MemberId;
use coffer_domain::time::Clock as _;
use coffer_server::auth::AuthError;
use coffer_server::auth::staging_fixture::STAGING_ADMIN_CREDENTIAL_ID;
use coffer_server::cli::serve_with;
use coffer_server::testkit::{TestServer, link_token};
use coffer_store::{NewMember, NewPasskey};
use tokio::net::TcpListener;
use tracing::instrument::WithSubscriber as _;
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use crate::serve::Captured;

const LOCALHOST: &[(&str, &str)] = &[("COFFER_PUBLIC_URL", "http://localhost:8080")];

async fn with_passkey(server: &TestServer, role: Role, credential_id: &[u8]) {
    let now = server.clock.now();
    let mut tx = server.store.begin().await.unwrap();
    let member = tx
        .insert_member(NewMember {
            id: MemberId::new(),
            username: Username::parse("someone").unwrap(),
            display_name: DisplayName::parse("Someone").unwrap(),
            role,
            created_at: now,
        })
        .await
        .unwrap();
    tx.insert_passkey(NewPasskey {
        member: member.id,
        label: "passkey".to_owned(),
        credential_id: credential_id.to_vec(),
        credential_json: "{}".to_owned(),
        created_at: now,
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// What `serve` says when told to stop as soon as it has started.
async fn serve_once(server: &TestServer) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    serve_with(server.app_state(), listener, async {})
        .await
        .map_err(|err| err.to_string())
}

#[tokio::test]
async fn the_seeded_admin_passkey_stops_serve_away_from_localhost() {
    let server = TestServer::new().await;
    with_passkey(&server, Role::Admin, STAGING_ADMIN_CREDENTIAL_ID).await;

    let refused = serve_once(&server).await.unwrap_err();

    assert!(refused.contains("staging seed"), "{refused}");
    assert!(refused.contains("COFFER_PUBLIC_URL"), "{refused}");
}

#[tokio::test]
async fn the_seed_serves_on_localhost() {
    let server = TestServer::with_env(LOCALHOST).await;
    with_passkey(&server, Role::Admin, STAGING_ADMIN_CREDENTIAL_ID).await;

    serve_once(&server).await.unwrap();
}

#[tokio::test]
async fn a_member_cannot_stop_the_server_by_registering_the_public_credential() {
    let server = TestServer::new().await;
    with_passkey(&server, Role::Member, STAGING_ADMIN_CREDENTIAL_ID).await;

    serve_once(&server).await.unwrap();
}

#[tokio::test]
async fn another_admin_passkey_serves_anywhere() {
    let server = TestServer::new().await;
    with_passkey(&server, Role::Admin, b"a real authenticator's id").await;

    serve_once(&server).await.unwrap();
}

/// The check looks for the id of the key the seed really uses.
#[test]
fn the_check_names_the_committed_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../staging/fixtures/passkeys.json"
    );
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let admin = fixture["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["role"] == "admin")
        .expect("the fixture has an admin");
    let id = URL_SAFE_NO_PAD
        .decode(admin["credentialId"].as_str().unwrap())
        .unwrap();
    assert_eq!(id, STAGING_ADMIN_CREDENTIAL_ID);
}

/// The x and y of a key in the fixture, the end of its PKCS#8 form.
fn fixture_key(username: &str) -> ([u8; 32], [u8; 32]) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../staging/fixtures/passkeys.json"
    );
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let member = fixture["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["username"] == username)
        .unwrap();
    let der = STANDARD
        .decode(member["privateKeyPkcs8"].as_str().unwrap())
        .unwrap();
    let point = &der[der.len() - 64..];
    (
        point[..32].try_into().unwrap(),
        point[32..].try_into().unwrap(),
    )
}

/// A key nobody seeded: P-256's base point, on the curve like any public key.
const OTHER_KEY: ([u8; 32], [u8; 32]) = (
    [
        0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5, 0x63, 0xa4, 0x40,
        0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0, 0xf4, 0xa1, 0x39, 0x45, 0xd8, 0x98,
        0xc2, 0x96,
    ],
    [
        0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e, 0xe7, 0xeb, 0x4a, 0x7c, 0x0f, 0x9e,
        0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e, 0xce, 0xcb, 0xb6, 0x40, 0x68, 0x37, 0xbf,
        0x51, 0xf5,
    ],
);
const OTHER_ID: &[u8] = b"an id nobody seeded";
const MEMBER_ID: &[u8] = b"coffer-staging-fixture-member-passkey";

/// SHA-256 of the relying party ids, as an authenticator signs them.
const RP_HASH_TEST: [u8; 32] = [
    0x74, 0xe6, 0x77, 0x4f, 0x58, 0x14, 0xe0, 0x70, 0x07, 0xef, 0x0e, 0x61, 0x9d, 0xa9, 0x0f, 0x1a,
    0x8c, 0x02, 0x98, 0x88, 0x35, 0x3a, 0x87, 0xd2, 0xa7, 0x4a, 0x50, 0x34, 0xb7, 0xfe, 0x86, 0xad,
];
const RP_HASH_LOCALHOST: [u8; 32] = [
    0x49, 0x96, 0x0d, 0xe5, 0x88, 0x0e, 0x8c, 0x68, 0x74, 0x34, 0x17, 0x0f, 0x64, 0x76, 0x60, 0x5b,
    0x8f, 0xe4, 0xae, 0xb9, 0xa2, 0x86, 0x32, 0xc7, 0x99, 0x5c, 0xf3, 0xba, 0x83, 0x1d, 0x97, 0x63,
];

fn cbor_bytes(bytes: &[u8]) -> Vec<u8> {
    let length = u16::try_from(bytes.len()).unwrap().to_be_bytes();
    [&[0x59], length.as_slice(), bytes].concat()
}

/// What a browser answers `options` with for this credential id and key, with no attestation.
fn credential(
    options: &impl serde::Serialize,
    id: &[u8],
    key: ([u8; 32], [u8; 32]),
) -> RegisterPublicKeyCredential {
    let options = serde_json::to_value(options).unwrap();
    let challenge = &options["publicKey"]["challenge"];
    let (rp_hash, origin) = if options["publicKey"]["rp"]["id"] == "localhost" {
        (RP_HASH_LOCALHOST, "http://localhost:8080")
    } else {
        (RP_HASH_TEST, "https://coffer.test")
    };
    let mut auth_data = rp_hash.to_vec();
    auth_data.extend([0x45, 0, 0, 0, 0]);
    auth_data.extend([0; 16]);
    auth_data.extend(u16::try_from(id.len()).unwrap().to_be_bytes());
    auth_data.extend(id);
    auth_data.extend([0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20]);
    auth_data.extend(key.0);
    auth_data.extend([0x22, 0x58, 0x20]);
    auth_data.extend(key.1);
    let mut attestation = vec![0xa3, 0x63];
    attestation.extend(b"fmt");
    attestation.push(0x64);
    attestation.extend(b"none");
    attestation.push(0x67);
    attestation.extend(b"attStmt");
    attestation.extend([0xa0, 0x68]);
    attestation.extend(b"authData");
    attestation.extend(cbor_bytes(&auth_data));
    let client_data = serde_json::json!({
        "type": "webauthn.create",
        "challenge": challenge,
        "origin": origin,
        "crossOrigin": false,
    })
    .to_string();
    let encoded_id = URL_SAFE_NO_PAD.encode(id);
    serde_json::from_value(serde_json::json!({
        "id": encoded_id,
        "rawId": encoded_id,
        "type": "public-key",
        "response": {
            "attestationObject": URL_SAFE_NO_PAD.encode(attestation),
            "clientDataJSON": URL_SAFE_NO_PAD.encode(client_data),
        },
        "extensions": {},
    }))
    .unwrap()
}

/// A first admin enrolling with this credential through the setup link.
async fn enroll_first_admin(
    server: &TestServer,
    id: &[u8],
    key: ([u8; 32], [u8; 32]),
) -> Result<MemberId, AuthError> {
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let (ceremony, options) = server
        .identity
        .begin_enrollment(
            &link_token(&link),
            Some(Username::parse("first").unwrap()),
            Some(DisplayName::parse("First").unwrap()),
        )
        .await
        .unwrap();
    server
        .identity
        .finish_enrollment(
            ceremony,
            TokenName::parse("passkey").unwrap(),
            credential(&options, id, key),
        )
        .await
}

fn is_public_passkey(result: &Result<MemberId, AuthError>) -> bool {
    matches!(result, Err(AuthError::PasskeyPublic))
}

#[tokio::test]
async fn away_from_localhost_each_seeded_id_and_key_is_refused_at_enrollment() {
    for (id, key) in [
        (STAGING_ADMIN_CREDENTIAL_ID, OTHER_KEY),
        (MEMBER_ID, OTHER_KEY),
        (OTHER_ID, fixture_key("alex")),
        (OTHER_ID, fixture_key("sam")),
    ] {
        let server = TestServer::new().await;
        assert!(is_public_passkey(
            &enroll_first_admin(&server, id, key).await
        ));
    }
}

#[tokio::test]
async fn away_from_localhost_another_key_enrolls() {
    let server = TestServer::new().await;

    enroll_first_admin(&server, OTHER_ID, OTHER_KEY)
        .await
        .unwrap();
}

#[tokio::test]
async fn on_localhost_the_seeded_key_enrolls() {
    let server = TestServer::with_env(LOCALHOST).await;

    enroll_first_admin(&server, STAGING_ADMIN_CREDENTIAL_ID, fixture_key("alex"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_member_promoted_after_trying_the_public_key_cannot_stop_the_server() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let admin_id = server.authenticator().enroll(&link, "admin", "Admin").await;
    let admin = Actor {
        member: admin_id,
        role: Role::Admin,
        via: Via::Session,
    };
    let invite = server
        .identity
        .create_invite(&admin, Role::Member, None)
        .await
        .unwrap();
    let member_id = server.authenticator().enroll(&invite, "bob", "Bob").await;
    let member = Actor {
        member: member_id,
        role: Role::Member,
        via: Via::Session,
    };

    let (ceremony, options) = server.identity.begin_add_passkey(&member).await.unwrap();
    let refused = server
        .identity
        .finish_add_passkey(
            &member,
            ceremony,
            TokenName::parse("public").unwrap(),
            credential(&options, STAGING_ADMIN_CREDENTIAL_ID, fixture_key("alex")),
        )
        .await;
    assert!(matches!(refused, Err(AuthError::PasskeyPublic)));
    server
        .identity
        .set_role(&admin, member_id, Role::Admin)
        .await
        .unwrap();

    serve_once(&server).await.unwrap();
}

/// The warning `serve` logs for the seed on this address, if any.
async fn warned_listening_on(server: &TestServer, address: &str) -> bool {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    let listener = TcpListener::bind(address).await.unwrap();
    serve_with(server.app_state(), listener, async {})
        .with_subscriber(subscriber)
        .await
        .unwrap();
    log.text().contains("anyone who can reach it")
}

#[tokio::test]
async fn the_seed_on_localhost_warns_on_an_address_others_can_reach() {
    let server = TestServer::with_env(LOCALHOST).await;
    with_passkey(&server, Role::Admin, STAGING_ADMIN_CREDENTIAL_ID).await;

    assert!(warned_listening_on(&server, "0.0.0.0:0").await);
    assert!(!warned_listening_on(&server, "127.0.0.1:0").await);
}
