//! Disconnecting a Plaid item, and removal when the credentials or the provider
//! are unusable.

use super::{ACCESS_MARKER, CREDENTIALS, GET_ACCOUNTS, ITEM_REMOVE, LOGIN_REQUIRED, REMOVE, world};
use axum::http::StatusCode;
use coffer_domain::identity::{Actor, Role, Via};
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::{TestServer, captured_log_text};
use coffer_store::ConnectionStatus;
use serde_json::json;

#[tokio::test]
async fn disconnect_removes_item_or_fails_cleanly() {
    let world = world().await;
    let before = world.connection().await;
    world
        .script
        .reply(REMOVE, StatusCode::INTERNAL_SERVER_ERROR, "");

    let (status, problem) = world.delete_connection().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(problem["code"], json!("provider_error"));
    let kept = world.connection().await;
    assert_eq!(kept.status, before.status);
    assert!(
        kept.credentials == before.credentials,
        "the credentials are kept"
    );

    world.script.reply(REMOVE, StatusCode::OK, ITEM_REMOVE);
    let (status, _) = world.delete_connection().await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    let removed = world.connection().await;
    assert_eq!(removed.status, ConnectionStatus::Removed);
    assert!(removed.credentials.is_none(), "the credentials are erased");
    let requests = world.script.seen(REMOVE);
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]["access_token"] == format!("access-{ACCESS_MARKER}").as_str(),
        "the removal names the Item's access token"
    );
}

/// Erasing the only access token while the Item lives on would leave it
/// billing with no way to remove it, so a Plaid connection stays until coffer
/// can give it up: here its sealed token cannot be opened.
#[tokio::test]
async fn removal_refused_when_the_plaid_token_cannot_be_opened() {
    let world = world().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_connection_credentials(world.connection, Some(vec![0; 64]))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = world.connection().await;

    let (status, problem) = world.delete_connection().await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("conflict"));
    let kept = world.connection().await;
    assert_eq!(kept.status, before.status);
    assert!(
        kept.credentials == before.credentials,
        "the sealed token is kept for when the key is restored"
    );
    assert_eq!(world.script.seen(REMOVE).len(), 0);
}

/// The same when Plaid is no longer configured on the server.
#[tokio::test]
async fn removal_refused_when_plaid_is_not_offered() {
    let mut server = TestServer::new().await;
    server.offer_no_provider();
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let connection = server
        .seed_connection(ProviderKind::Plaid, "Plaid bank", member, CREDENTIALS)
        .await;

    let refused = server.sync.remove_connection(&alice, connection).await;

    assert!(
        matches!(refused, Err(SyncError::Conflict(_))),
        "the removal is refused"
    );
    let kept = server.store.get_connection(connection).await.unwrap();
    assert_ne!(kept.status, ConnectionStatus::Removed);
    assert!(kept.credentials.is_some(), "the sealed token is kept");
}

/// An Item already gone at Plaid must not keep its connection forever.
#[tokio::test]
async fn disconnect_of_an_item_already_gone_succeeds() {
    let world = world().await;
    world.script.reply(
        REMOVE,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": "ITEM_ERROR", "error_code": "ITEM_NOT_FOUND" }).to_string(),
    );

    let (status, _) = world.delete_connection().await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(world.connection().await.status, ConnectionStatus::Removed);
}

/// Credentials that do not parse cannot be used at Plaid either: they do not
/// stop a removal, and a sync says to reconnect without quoting them.
#[tokio::test]
async fn unusable_credentials_fail_sync_and_do_not_block_removal() {
    let world = world().await;
    let garbled = world
        .server
        .seed_connection(
            ProviderKind::Plaid,
            "Garbled",
            world.alice.member,
            "not-json-ACCESSTOKEN-MARKER-71",
        )
        .await;

    let run = world.sync_connection(garbled).await;

    assert_eq!(run.status, SyncRunStatus::Failed);
    let detail = run.detail.unwrap();
    assert!(detail.contains("reconnect"), "the run says what to do");
    assert!(!detail.contains(ACCESS_MARKER), "and quotes nothing");
    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 0);

    world
        .server
        .sync
        .remove_connection(&world.alice, garbled)
        .await
        .unwrap();
    let log = captured_log_text();
    let warned = log.lines().any(|line| {
        line.contains("removed without disconnecting") && line.contains(&garbled.to_string())
    });
    assert!(warned, "a removal that gave up nothing at Plaid is logged");
    assert!(!log.contains(ACCESS_MARKER), "and quotes nothing");
}

/// `/item/remove` must not report success while the Item may keep billing: an
/// invalid token can also mean a wrong environment or wrong keys, and an Item
/// that needs a new login still exists.
#[tokio::test]
async fn removal_failures_leave_the_connection_unchanged() {
    for body in [
        json!({ "error_type": "INVALID_INPUT", "error_code": "INVALID_ACCESS_TOKEN" }).to_string(),
        LOGIN_REQUIRED.to_owned(),
        json!({ "error_type": "ITEM_ERROR", "error_code": "USER_PERMISSION_REVOKED" }).to_string(),
    ] {
        let world = world().await;
        let before = world.connection().await;
        world.script.reply(REMOVE, StatusCode::BAD_REQUEST, body);

        let (status, _) = world.delete_connection().await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let kept = world.connection().await;
        assert_eq!(kept.status, before.status);
        assert!(
            kept.credentials == before.credentials,
            "the credentials are kept"
        );
    }
}
