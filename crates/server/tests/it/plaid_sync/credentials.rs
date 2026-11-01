//! The access token: sealed at connect, never shown, and refused on reconnect.

use super::{ACCESS_MARKER, EXCHANGE, GET_ACCOUNTS, SYNC, page, world};
use axum::http::StatusCode;
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::sync::SyncRunStatus;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::{ConnectInput, ProviderError, ProviderKind};
use coffer_server::testkit::{captured_log_text, log_marker};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};

#[tokio::test]
async fn connect_seals_the_access_token_and_never_shows_it() {
    let world = world().await;
    let marker = log_marker();
    world.script.reply(
        EXCHANGE,
        StatusCode::OK,
        json!({ "access_token": "access-NEWTOKEN-MARKER-5", "item_id": "item-9" }).to_string(),
    );

    let view = world
        .server
        .sync
        .create_connection(
            &world.alice,
            ProviderKind::Plaid,
            ConnectionLabel::parse("New").unwrap(),
            ConnectInput::PlaidPublicToken(SecretString::from("public-PUBLICTOKEN-MARKER-6")),
            None,
        )
        .await
        .unwrap();

    let stored = world.server.store.get_connection(view.id).await.unwrap();
    assert_eq!(stored.provider, "plaid");
    let sealed = stored.credentials.unwrap();
    assert!(
        !String::from_utf8_lossy(&sealed).contains("NEWTOKEN-MARKER"),
        "the stored bytes do not hold the token in clear"
    );
    let opened = world.server.vault.open(view.id, &sealed.into()).unwrap();
    let credentials: Value = serde_json::from_str(opened.expose_secret()).unwrap();
    assert!(
        credentials["access_token"] == "access-NEWTOKEN-MARKER-5",
        "the credentials hold the access token"
    );
    assert!(
        credentials["item_id"] == "item-9",
        "the credentials hold the Item's id"
    );
    assert!(
        world.script.seen(EXCHANGE)[0]["public_token"] == "public-PUBLICTOKEN-MARKER-6",
        "the exchange names the public token"
    );
    let audit = format!("{:?}", world.audit().await);
    let view_text = serde_json::to_string(&view).unwrap();
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds this test");
    for text in [audit, view_text, log] {
        assert!(!text.contains("NEWTOKEN-MARKER"), "the token is not shown");
        assert!(
            !text.contains("PUBLICTOKEN-MARKER"),
            "the public token is not shown"
        );
    }
}

/// A sync's every outcome leaves the seeded access token out of the run, the
/// connection, the audit log and the logs.
#[tokio::test]
async fn access_token_absent_from_outcomes() {
    let world = world().await;
    let marker = log_marker();
    let echoed = format!("access-{ACCESS_MARKER}");
    world.script.reply(
        GET_ACCOUNTS,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": echoed, "error_code": echoed, "error_message": echoed }).to_string(),
    );
    let first = world.sync().await;
    world
        .script
        .reply(GET_ACCOUNTS, StatusCode::BAD_GATEWAY, echoed.clone());
    let second = world.sync().await;
    world.script.page(echoed.clone());
    let third = world.sync().await;

    assert_eq!(world.script.seen(GET_ACCOUNTS).len(), 3, "Plaid was asked");
    assert_eq!(world.script.seen(SYNC).len(), 1, "each answer was reached");
    for run in [&first, &second, &third] {
        assert_eq!(run.status, SyncRunStatus::Failed);
    }
    let connection = world.connection().await;
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds this test");
    for text in [
        format!("{first:?}"),
        format!("{second:?}"),
        format!("{third:?}"),
        format!("{:?}", connection.last_error),
        format!("{:?}", world.audit().await),
        log,
    ] {
        assert!(!text.contains(ACCESS_MARKER), "the token is not shown");
    }
}

/// New credentials would name another Item and leave the old one billing at
/// Plaid: a Plaid connection logs in again through update mode only, so no
/// input reconnects it, nothing reaches Plaid and the cursor stays.
#[tokio::test]
async fn reconnect_refuses_every_input_and_keeps_the_cursor() {
    let world = world().await;
    world.script.page(page(&[], &[], &[], "old-item", false));
    world.sync().await;
    let before = world.connection().await;

    for input in [
        ConnectInput::SimpleFinSetupToken(SecretString::from("x")),
        ConnectInput::PlaidPublicToken(SecretString::from("public-2")),
    ] {
        let refused = world
            .server
            .sync
            .reconnect(&world.alice, world.connection, input)
            .await;
        assert!(
            matches!(refused, Err(SyncError::Provider(ProviderError::Invalid(_)))),
            "update mode is the only way back"
        );
    }

    assert_eq!(
        world.script.seen(EXCHANGE).len(),
        0,
        "nothing was exchanged"
    );
    let after = world.connection().await;
    assert_eq!(after.provider_state.as_deref(), Some("old-item"));
    assert!(
        after.credentials == before.credentials,
        "the credentials are kept"
    );
}
