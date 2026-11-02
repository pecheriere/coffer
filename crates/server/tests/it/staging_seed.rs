//! `serve` refuses the staging seed, whose admin passkey is public, away from localhost.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_domain::identity::{DisplayName, Role, Username};
use coffer_domain::ids::MemberId;
use coffer_domain::time::Clock as _;
use coffer_server::cli::{STAGING_ADMIN_CREDENTIAL_ID, serve_with};
use coffer_server::testkit::TestServer;
use coffer_store::{NewMember, NewPasskey};
use tokio::net::TcpListener;

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
