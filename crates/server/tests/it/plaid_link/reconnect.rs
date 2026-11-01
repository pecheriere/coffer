//! Update mode for a connection that needs a new login, and confirming it.

use super::{
    ACCESS_MARKER, ACCESS_TOKEN, CURSOR, DEFAULT_CSP, LINK_TOKEN, LINK_TOKEN_CREATE, World,
    confirm_path, csp, reconnect_path, tags, world,
};
use axum::http::StatusCode;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::Browser;
use coffer_server::testkit::{TEST_PUBLIC_URL, captured_log_text, log_marker};
use coffer_store::{ConnectionStatus, ConnectionStatusChange};
use serde_json::json;

#[tokio::test]
async fn reconnect_uses_update_mode() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.script.link_token();
    let marker = log_marker();

    let page = world.admin.get(&reconnect_path(id)).await;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains(&format!("data-link-token=\"{LINK_TOKEN}\""))
    );
    assert!(
        page.body
            .contains(&format!("data-submit=\"{}\"", confirm_path(id)))
    );
    assert!(
        !tags(&page.body, "input")
            .iter()
            .any(|input| input.contains("plaid-label")),
        "update mode names no new connection"
    );
    let seen = world.script.seen(LINK_TOKEN_CREATE);
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]["access_token"] == ACCESS_TOKEN,
        "update mode names the Item"
    );
    assert!(seen[0].get("products").is_none());
    assert!(seen[0].get("transactions").is_none());
    assert!(
        !page.body.contains(ACCESS_MARKER),
        "the access token stays on the server"
    );
    let log = captured_log_text();
    assert!(log.contains(&marker));
    assert!(!log.contains(ACCESS_MARKER), "the token must not be logged");
}

#[tokio::test]
async fn confirming_reauthorization_keeps_credentials_and_cursor() {
    let mut world = world().await;
    let id = world.needing_login().await;
    let before = world.server.store.get_connection(id).await.unwrap();

    let reply = world.confirm(id).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "redirect": "/connections" }));
    let after = world.server.store.get_connection(id).await.unwrap();
    assert_eq!(after.status, ConnectionStatus::Active);
    assert!(after.last_error.is_none(), "the error is cleared");
    assert!(
        after.credentials == before.credentials,
        "nothing is resealed: update mode keeps the access token"
    );
    assert_eq!(
        after.provider_state.as_deref(),
        Some(CURSOR),
        "the same Item keeps its cursor"
    );
    let runs = world.runs(id).await;
    assert_eq!(runs.len(), 1, "a run is queued");
    assert_eq!(runs[0].trigger, SyncTrigger::Manual);
    let audit = world.audit_text().await;
    assert!(audit.contains("connection.reconnect"), "it is audited");
    assert!(
        audit.contains(r#""credentials_replaced":{"before":null,"after":false}"#),
        "and says the credentials were not replaced"
    );
    assert!(!audit.contains(ACCESS_MARKER));
    assert_eq!(
        world.script.seen(LINK_TOKEN_CREATE).len(),
        0,
        "confirming asks nothing of Plaid"
    );
}

#[tokio::test]
async fn confirming_reauthorization_needs_csrf_and_same_origin() {
    let mut world = world().await;
    let id = world.needing_login().await;
    let path = confirm_path(id);
    let csrf = World::csrf(&mut world.admin).await;

    let missing = world
        .admin
        .post_json_with(&path, Some(TEST_PUBLIC_URL), None, &json!({}))
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let cross = world
        .admin
        .post_json_with(&path, Some("https://evil.example"), Some(&csrf), &json!({}))
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    let mut anonymous = Browser::new(&world.server);
    let unauthenticated = anonymous
        .post_json_with(&path, Some(TEST_PUBLIC_URL), Some(&csrf), &json!({}))
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);

    let current = world.server.store.get_connection(id).await.unwrap();
    assert_eq!(current.status, ConnectionStatus::ReauthRequired);
}

#[tokio::test]
async fn member_cannot_reconnect_others_connection() {
    let world = world().await;
    let id = world.needing_login().await;
    let mut bob = world.bob().await;
    let csrf = World::csrf(&mut bob).await;

    let page = bob.get(&reconnect_path(id)).await;
    assert_eq!(page.status, StatusCode::FORBIDDEN);
    assert_eq!(csp(&page), DEFAULT_CSP);
    assert!(!page.body.contains("data-link-token"));
    let confirm = bob
        .post_json_with(
            &confirm_path(id),
            Some(TEST_PUBLIC_URL),
            Some(&csrf),
            &json!({}),
        )
        .await;
    assert_eq!(confirm.status, StatusCode::FORBIDDEN);

    assert_eq!(
        world.script.seen(LINK_TOKEN_CREATE).len(),
        0,
        "no link token for a connection the member may not manage"
    );
    let current = world.server.store.get_connection(id).await.unwrap();
    assert_eq!(current.status, ConnectionStatus::ReauthRequired);
    assert_eq!(world.runs(id).await.len(), 0);
    let listed = bob.get("/connections").await;
    assert!(
        !listed.body.contains(&reconnect_path(id)),
        "no Reconnect link either"
    );
}

#[tokio::test]
async fn only_live_plaid_connections_can_be_reconfirmed() {
    let mut world = world().await;
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Bridge",
            world.alice.member,
            "https://u:p@bridge.example/simplefin",
        )
        .await;
    let removed = world.seeded().await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_connection_credentials(removed, None).await.unwrap();
    tx.update_connection_status(
        removed,
        ConnectionStatusChange {
            status: ConnectionStatus::Removed,
            last_error: None,
            attempted_at: None,
            succeeded_at: None,
            at: world.server.clock.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let unknown = ConnectionId::new();

    for (id, status) in [
        (other, StatusCode::NOT_FOUND),
        (unknown, StatusCode::NOT_FOUND),
        (removed, StatusCode::CONFLICT),
    ] {
        assert_eq!(world.admin.get(&reconnect_path(id)).await.status, status);
        assert_eq!(world.confirm(id).await.status, status);
    }
    assert_eq!(
        world
            .admin
            .get("/connections/not-an-id/plaid/reconnect")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.script.seen(LINK_TOKEN_CREATE).len(),
        0,
        "nothing reached Plaid"
    );
    let bridge = world.server.store.get_connection(other).await.unwrap();
    assert_eq!(bridge.status, ConnectionStatus::Active);
    assert_eq!(world.runs(other).await.len(), 0);
}

#[tokio::test]
async fn confirming_reauthorization_needs_write_on_every_account() {
    let world = world().await;
    let id = world.needing_login().await;
    let (record, _) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("narrow").unwrap(),
            Scope::Admin,
            None,
            vec![TokenGrant {
                target: GrantTarget::All,
                level: AccountLevel::Transactions,
            }],
        )
        .await
        .unwrap();
    let narrow = Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: record.id,
            scope: Scope::Admin,
        },
    };

    let confirmed = world.server.sync.confirm_reauthorized(&narrow, id).await;
    assert!(
        matches!(confirmed, Err(SyncError::Denied(_))),
        "a token below write on every account is refused"
    );
    let token = world.server.sync.update_link_token(&narrow, id).await;
    assert!(
        matches!(token, Err(SyncError::Denied(_))),
        "so is update mode"
    );
    let new = world.server.sync.new_link_token(&narrow).await;
    assert!(matches!(new, Err(SyncError::Denied(_))), "and a new Item");
    assert_eq!(world.script.seen(LINK_TOKEN_CREATE).len(), 0);

    let current = world.server.store.get_connection(id).await.unwrap();
    assert_eq!(current.status, ConnectionStatus::ReauthRequired);
}
