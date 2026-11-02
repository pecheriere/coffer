//! Forgetting a Plaid connection coffer can no longer give up: offered only
//! when removal is refused for that reason, after typing the connection's
//! label, by a passkey session, and audited as a forgetting.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse as _, Response};
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AgentTokenId, ConnectionId};
use coffer_domain::policy::{Action, DenyReason};
use coffer_domain::sync::SyncRunStatus;
use coffer_domain::sync::SyncTrigger;
use coffer_server::http::router;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_server::testkit::{CallGate, MockUpstream, TestServer};
use coffer_store::{AuditEvent, AuditFilter, ConnectionRecord, ConnectionStatus, PageRequest};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use tower::ServiceExt as _;

const CREDENTIALS: &str = r#"{"access_token":"access-FORGET-MARKER-81","item_id":"item-1"}"#;
const ACCOUNTS: &str = include_str!("fixtures/plaid/accounts_get.json");
const POSTED: &str = include_str!("fixtures/plaid/transactions_sync_posted.json");
const LABEL: &str = "Plaid bank";
const WRONG: &str = "WRONG-LABEL-MARKER-82";
const WARNING: &str = "If this could be a wrong key or a restored backup, fix that first: \
     forgetting can&#39;t be undone, and removal then works normally.";
const NO_END: &str = "coffer can&#39;t end billing for this Item, because that needs the token. \
     Contact Plaid, or check your Plaid dashboard, to end it.";

struct World {
    server: TestServer,
    admin: Browser,
    alice: Actor,
    mock: MockUpstream,
}

/// Admin "alice" signed in, on a server configured for Plaid, whose mock
/// answers nothing: any call to Plaid would show in `mock.requests()`.
async fn world() -> World {
    let (server, signed_in, _) = with_admin().await;
    let mock = MockUpstream::start(Router::new()).await;
    finish(server.with_plaid(&mock), signed_in, mock).await
}

async fn finish(server: TestServer, signed_in: Browser, mock: MockUpstream) -> World {
    let mut admin = Browser::new(&server);
    admin.cookie.clone_from(&signed_in.cookie);
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    World {
        server,
        admin,
        alice,
        mock,
    }
}

impl World {
    /// A Plaid connection whose sealed token no longer opens.
    async fn lost_key(&self) -> ConnectionId {
        let id = self.healthy(LABEL).await;
        self.corrupt(id).await;
        id
    }

    async fn healthy(&self, label: &str) -> ConnectionId {
        self.server
            .seed_connection(ProviderKind::Plaid, label, self.alice.member, CREDENTIALS)
            .await
    }

    async fn corrupt(&self, id: ConnectionId) {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.set_connection_credentials(id, Some(vec![0; 64]))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn connection(&self, id: ConnectionId) -> ConnectionRecord {
        self.server.store.get_connection(id).await.unwrap()
    }

    async fn audit(&self, action: &str) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|event| event.action == action)
            .collect()
    }

    async fn forget(&mut self, id: ConnectionId, confirmation: Option<&str>) -> Reply {
        let csrf = self.admin.get("/connections").await.csrf();
        let mut fields = vec![("csrf", csrf.as_str())];
        if let Some(confirmation) = confirmation {
            fields.push(("confirmation", confirmation));
        }
        self.admin
            .post_form(&format!("/connections/{id}/forget"), &fields)
            .await
    }

    /// Asserts the connection is exactly as `before` and nothing was audited.
    async fn assert_kept(&self, id: ConnectionId, before: &ConnectionRecord) {
        let kept = self.connection(id).await;
        assert_eq!(kept.status, before.status, "the status is unchanged");
        assert!(
            kept.credentials == before.credentials,
            "the sealed token is kept"
        );
        assert_eq!(self.audit("connection.forget").await.len(), 0);
        assert_eq!(self.audit("connection.remove").await.len(), 0);
        assert_eq!(self.mock.requests().len(), 0, "Plaid is not called");
    }
}

fn forget_path(id: ConnectionId) -> String {
    format!("/connections/{id}/forget")
}

#[tokio::test]
async fn forgetting_is_offered_only_where_removal_is_refused() {
    let mut world = world().await;
    let healthy = world.healthy("Healthy bank").await;
    let lost = world.lost_key().await;
    // Not a connection that bills until removed: removal goes ahead for it.
    let simplefin = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Simple bank",
            world.alice.member,
            "https://u:p@bridge.example/simplefin",
        )
        .await;
    world.corrupt(simplefin).await;

    let page = world.admin.get("/connections").await;

    assert!(page.body.contains(&forget_path(lost)));
    assert!(!page.body.contains(&forget_path(healthy)));
    assert!(!page.body.contains(&forget_path(simplefin)));
    assert!(
        page.body.contains(WARNING) || page.body.contains(&WARNING.replace("&#39;", "'")),
        "the page warns that a wrong key or restored backup comes first"
    );
    assert!(
        page.body.contains(NO_END) || page.body.contains(&NO_END.replace("&#39;", "'")),
        "the page says what coffer cannot do and who to contact"
    );

    // Once forgotten, it is not offered again.
    let done = world.forget(lost, Some(LABEL)).await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    let after = world.admin.get("/connections").await;
    assert!(!after.body.contains(&forget_path(lost)));
}

/// With Plaid not configured the token still opens, so nothing is forgotten:
/// the member configures Plaid again and removes the connection.
#[tokio::test]
async fn nothing_is_forgotten_while_plaid_is_merely_not_configured() {
    let (mut server, signed_in, _) = with_admin().await;
    server.offer_no_provider();
    let mock = MockUpstream::start(Router::new()).await;
    let mut world = finish(server, signed_in, mock).await;
    let id = world.healthy(LABEL).await;
    let before = world.connection(id).await;

    let page = world.admin.get("/connections").await;
    assert!(!page.body.contains(&forget_path(id)));
    assert!(
        page.body
            .contains("Configure it again to remove this connection.")
    );
    let refused = world.forget(id, Some(LABEL)).await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    world.assert_kept(id, &before).await;
    let removal = world.server.sync.remove_connection(&world.alice, id).await;
    assert!(
        matches!(removal, Err(SyncError::Conflict(_))),
        "removal stays refused"
    );
}

#[tokio::test]
async fn forgetting_needs_the_connections_label_typed() {
    let mut world = world().await;
    let id = world.lost_key().await;
    let before = world.connection(id).await;

    for typed in [None, Some(""), Some("   "), Some("plaid bank"), Some(WRONG)] {
        let refused = world.forget(id, typed).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("Type the connection's label"));
        assert!(!refused.body.contains(WRONG), "the page does not echo it");
        world.assert_kept(id, &before).await;
    }
}

#[tokio::test]
async fn a_connection_that_can_be_removed_cannot_be_forgotten() {
    let mut world = world().await;
    let id = world.healthy(LABEL).await;
    let before = world.connection(id).await;

    let refused = world.forget(id, Some(LABEL)).await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
    world.assert_kept(id, &before).await;
}

#[tokio::test]
async fn forgetting_erases_the_token_and_audits_it_as_forgotten() {
    let mut world = world().await;
    let id = world.lost_key().await;

    let done = world.forget(id, Some(LABEL)).await;

    assert_eq!(done.status, StatusCode::SEE_OTHER);
    let forgotten = world.connection(id).await;
    assert_eq!(forgotten.status, ConnectionStatus::Removed);
    assert!(
        forgotten.credentials.is_none(),
        "the sealed token is erased"
    );
    assert_eq!(world.mock.requests().len(), 0, "Plaid is not called");
    assert_eq!(
        world.audit("connection.remove").await.len(),
        0,
        "it is not recorded as a removal"
    );
    let entries = world.audit("connection.forget").await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.entity_type, "connection");
    assert_eq!(entry.entity_id, id.to_string());
    let changes: Vec<_> = entry.changes.iter().collect();
    let status = changes.iter().find(|(name, _)| *name == "status").unwrap();
    assert_eq!(
        status.1.after,
        AuditValue::from("removed"),
        "the status becomes removed"
    );
    let given_up = changes
        .iter()
        .find(|(name, _)| *name == "given_up_at_provider")
        .unwrap();
    assert_eq!(
        given_up.1.after,
        AuditValue::from(false),
        "it says the Item was not given up"
    );
    let reason = changes.iter().find(|(name, _)| *name == "reason").unwrap();
    assert_eq!(
        reason.1.after,
        AuditValue::from("sealed token cannot be opened"),
        "the audit entry records why it was allowed"
    );
    let text = serde_json::to_string(&entry.changes).unwrap();
    assert!(!text.contains("FORGET-MARKER"), "no credential is audited");

    // A second attempt finds it already removed.
    let again = world.forget(id, Some(LABEL)).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(world.audit("connection.forget").await.len(), 1);
}

#[tokio::test]
async fn forgetting_needs_the_anti_forgery_token() {
    let mut world = world().await;
    let id = world.lost_key().await;
    let before = world.connection(id).await;
    let path = forget_path(id);

    let missing = world
        .admin
        .post_form(&path, &[("confirmation", LABEL)])
        .await;
    let wrong = world
        .admin
        .post_form(&path, &[("csrf", "AAAA"), ("confirmation", LABEL)])
        .await;

    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    world.assert_kept(id, &before).await;
}

#[tokio::test]
async fn an_agent_token_cannot_forget_and_the_api_delete_stays_refused() {
    let world = world().await;
    let id = world.lost_key().await;
    let before = world.connection(id).await;
    let agent = Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Admin,
        },
    };

    let err = world
        .server
        .sync
        .forget_connection(&agent, id, LABEL)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            SyncError::Denied(denied)
                if denied.reason == DenyReason::SessionRequired
                    && denied.action == Action::ForgetConnection { owner: world.alice.member }
        ),
        "a token is denied for want of a session"
    );
    assert!(
        world
            .server
            .sync
            .forgettable(&agent)
            .await
            .unwrap()
            .is_empty(),
        "a token is offered nothing to forget"
    );
    assert!(
        world
            .server
            .sync
            .forgettable(&world.alice)
            .await
            .unwrap()
            .contains(&id),
        "a session is"
    );

    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let request = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/v1/connections/{id}"))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .body(Body::empty())
        .unwrap();
    let response = router(world.server.app_state())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    drop(response.into_body().collect().await.unwrap());
    assert_eq!(status, StatusCode::CONFLICT);
    world.assert_kept(id, &before).await;
}

#[tokio::test]
async fn a_member_cannot_forget_anothers_connection() {
    let world = world().await;
    let id = world.lost_key().await;
    let before = world.connection(id).await;
    let invite = world
        .server
        .identity
        .create_invite(&world.alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&world.server);
    let mut key = world.server.authenticator();
    let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK);

    let page = bob.get("/connections").await;
    assert!(!page.body.contains(&forget_path(id)));
    let refused = bob
        .post_form(
            &forget_path(id),
            &[("csrf", &page.csrf()), ("confirmation", LABEL)],
        )
        .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    world.assert_kept(id, &before).await;
}

/// A forgetting while a run fetches leaves the run writing nothing: the apply
/// step re-checks the connection in the transaction that writes.
#[tokio::test]
async fn forgetting_during_a_sync_writes_nothing() {
    let gate = Arc::new(Mutex::new(None::<CallGate>));
    let (server, signed_in, _) = with_admin().await;
    let mock =
        MockUpstream::start(Router::new().fallback(gated).with_state(Arc::clone(&gate))).await;
    let world = finish(server.with_plaid(&mock), signed_in, mock).await;
    let id = world.healthy(LABEL).await;
    let held = CallGate::default();
    *gate.lock().unwrap() = Some(held.clone());
    let run = world
        .server
        .sync
        .request_sync(Some(&world.alice), id, SyncTrigger::Manual)
        .await
        .unwrap();
    let sync = Arc::clone(&world.server.sync);
    let running = tokio::spawn(async move { sync.run(run.id).await });

    held.entered().await;
    world.corrupt(id).await;
    world
        .server
        .sync
        .forget_connection(&world.alice, id, LABEL)
        .await
        .unwrap();
    held.release();
    let finished = running.await.unwrap().unwrap();

    assert_eq!(finished.status, SyncRunStatus::Failed);
    assert_eq!(
        finished.detail.as_deref(),
        Some("the connection is removed")
    );
    let stored = world.connection(id).await;
    assert_eq!(stored.status, ConnectionStatus::Removed);
    assert_eq!(stored.last_success_at, None);
    assert!(stored.credentials.is_none());
    assert_eq!(
        world
            .server
            .store
            .list_provider_accounts(None)
            .await
            .unwrap()
            .len(),
        0,
        "no provider account was written"
    );
}

/// Plaid's answers for a run: the fixtures' accounts and one page of
/// changes, the first request held while a gate is set.
async fn gated(State(gate): State<Arc<Mutex<Option<CallGate>>>>, uri: Uri) -> Response {
    let held = gate.lock().unwrap().take();
    if let Some(held) = held {
        held.hold().await;
    }
    let body = match uri.path() {
        "/accounts/get" => ACCOUNTS,
        "/transactions/sync" => POSTED,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}
