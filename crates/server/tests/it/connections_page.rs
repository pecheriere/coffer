//! The `/connections` page: connections and their provider accounts
//! without financial data, and forms that need the anti-forgery token,
//! never echo a setup token and report limits as messages.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::identity::{Actor, Role};
use coffer_domain::ids::ConnectionId;
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_server::testkit::{MockUpstream, TestServer, captured_log_text, log_marker};
use coffer_store::{ConnectionStatus, NewSyncRun, PageRequest};

const ACCOUNTS_FIRST: &str = include_str!("fixtures/simplefin/accounts_first.json");
const USER: &str = "sfuser-page";
const PASSWORD: &str = "SFPAGE-MARKER";
const BOGUS_TOKEN: &str = "SECRETTOKEN-page-not-a-real-setup-token";
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                   frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

/// What the mock bridge answers, in order: claims, then accounts.
#[derive(Default)]
struct Script {
    claims: Mutex<VecDeque<String>>,
    bridge_replies: Mutex<VecDeque<String>>,
}

fn answer(queue: &Mutex<VecDeque<String>>, headers: &HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    match queue.lock().unwrap().pop_front() {
        Some(body) => body.replace("{host}", &host).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn bridge(script: Arc<Script>) -> Router {
    let claims = Arc::clone(&script);
    Router::new()
        .route(
            "/claim/{id}",
            post(move |headers: HeaderMap| async move { answer(&claims.claims, &headers) }),
        )
        .route(
            "/simplefin/accounts",
            get(move |headers: HeaderMap| async move { answer(&script.bridge_replies, &headers) }),
        )
}

struct World {
    server: TestServer,
    admin: Browser,
    alice: Actor,
    mock: MockUpstream,
    script: Arc<Script>,
}

/// Admin "alice" signed in, on a server whose `SimpleFin` provider talks
/// to a mock bridge.
async fn world() -> World {
    let (server, signed_in, _) = with_admin().await;
    let script = Arc::new(Script::default());
    let mock = MockUpstream::start(bridge(Arc::clone(&script))).await;
    let server = server.with_simplefin(&mock);
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
        script,
    }
}

impl World {
    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.mock.base.port().unwrap())
    }

    fn setup_token(&self) -> String {
        STANDARD.encode(format!("http://{}/claim/x", self.host()))
    }

    fn access_url(&self) -> String {
        format!("http://{USER}:{PASSWORD}@{}/simplefin", self.host())
    }

    async fn seeded(&self, owner: Actor) -> ConnectionId {
        self.server
            .seed_connection(
                ProviderKind::SimpleFin,
                "Family bank",
                owner.member,
                &self.access_url(),
            )
            .await
    }

    async fn run_queued(&self, connection: ConnectionId) {
        let runs = self
            .server
            .store
            .list_sync_runs(connection, &PageRequest::default())
            .await
            .unwrap()
            .items;
        for run in runs {
            if run.started_at.is_none() {
                self.server.sync.run(run.id).await.unwrap();
            }
        }
    }

    async fn sync_fixture(&self, connection: ConnectionId) {
        self.script
            .bridge_replies
            .lock()
            .unwrap()
            .push_back(ACCOUNTS_FIRST.to_owned());
        let mut tx = self.server.store.begin().await.unwrap();
        tx.insert_sync_run(NewSyncRun {
            connection,
            trigger: SyncTrigger::Manual,
            requested_by: Some((self.alice.member, None)),
            queued_at: self.server.clock.now(),
        })
        .await
        .unwrap();
        tx.commit().await.unwrap();
        self.run_queued(connection).await;
    }

    async fn status(&self, connection: ConnectionId) -> ConnectionStatus {
        self.server
            .store
            .get_connection(connection)
            .await
            .unwrap()
            .status
    }

    async fn run_count(&self, connection: ConnectionId) -> usize {
        self.server
            .store
            .list_sync_runs(connection, &PageRequest::default())
            .await
            .unwrap()
            .items
            .len()
    }
}

/// The opening tags `<name …>` in `html`.
fn tags<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    html.match_indices(&open)
        .filter(|(at, _)| {
            html[at + open.len()..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
        })
        .map(|(at, _)| {
            let end = html[at..].find('>').map_or(html.len(), |len| at + len + 1);
            &html[at..end]
        })
        .collect()
}

fn assert_inline_free(reply: &Reply) {
    assert_eq!(reply.header(header::CONTENT_SECURITY_POLICY), CSP);
    assert!(!reply.body.contains("style="));
    assert_eq!(tags(&reply.body, "style").len(), 0);
    assert!(
        tags(&reply.body, "script")
            .iter()
            .all(|script| script.contains(" src=\""))
    );
}

#[tokio::test]
async fn connections_page_lists_without_balances() {
    let mut world = world().await;
    let empty = world.admin.get("/connections").await;
    assert_eq!(empty.status, StatusCode::OK);
    assert!(empty.body.contains("illustration-empty"));
    assert!(empty.body.contains("aria-current=\"page\">Connections<"));

    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let page = world.admin.get("/connections").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    for expected in [
        "Family bank",
        "simplefin",
        "Checking",
        "Credit Card",
        "Example Bank",
        "Not bound",
    ] {
        assert!(page.body.contains(expected), "missing {expected}");
    }
    for figure in [
        "2450.17", "2,450.17", "2400.17", "312.40", "42.50", "1500.00",
    ] {
        assert!(!page.body.contains(figure), "balance figure shown");
    }
    for description in ["GROCERY MARKET", "COFFEE ROASTERS"] {
        assert!(!page.body.contains(description), "transaction shown");
    }
    assert!(!page.body.contains(PASSWORD));
    for action in [
        "/connections/simplefin",
        &format!("/connections/{connection}/sync"),
        &format!("/connections/{connection}/remove"),
    ] {
        assert!(page.body.contains(&format!("action=\"{action}\"")));
    }
    // Reconnect is offered only to a connection that needs it.
    assert!(!page.body.contains("/simplefin/reconnect"));
}

#[tokio::test]
async fn add_form_creates_connection() {
    let mut world = world().await;
    world
        .script
        .claims
        .lock()
        .unwrap()
        .push_back(format!("http://{USER}:{PASSWORD}@{{host}}/simplefin"));
    let csrf = world.admin.get("/connections").await.csrf();

    let token = world.setup_token();
    let created = world
        .admin
        .post_form(
            "/connections/simplefin",
            &[
                ("csrf", &csrf),
                ("label", "Joint bank"),
                ("setup_token", &token),
            ],
        )
        .await;

    assert_eq!(created.status, StatusCode::SEE_OTHER);
    assert_eq!(created.header(header::LOCATION), "/connections");
    let connections = world.server.store.list_connections().await.unwrap();
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].label.as_str(), "Joint bank");
    assert_eq!(connections[0].owner, world.alice.member);
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains("Joint bank"));
    assert!(!page.body.contains(PASSWORD));
}

#[tokio::test]
async fn add_form_never_echoes_invalid_token() {
    let mut world = world().await;
    let marker = log_marker();
    let csrf = world.admin.get("/connections").await.csrf();

    let refused = world
        .admin
        .post_form(
            "/connections/simplefin",
            &[
                ("csrf", &csrf),
                ("label", "Joint bank"),
                ("setup_token", BOGUS_TOKEN),
            ],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_inline_free(&refused);
    assert!(refused.body.contains("role=\"alert\""));
    assert!(refused.body.contains("setup token"));
    assert!(!refused.body.contains("SECRETTOKEN"));
    let log = captured_log_text();
    assert!(
        log.contains(&marker),
        "the capture still holds this test's lines"
    );
    assert!(!log.contains("SECRETTOKEN"));
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        0
    );
    assert_eq!(world.mock.requests().len(), 0);
}

#[tokio::test]
async fn csrf_required_on_every_form() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let token = world.setup_token();
    for (path, fields) in [
        (
            "/connections/simplefin".to_owned(),
            vec![("label", "Joint bank"), ("setup_token", token.as_str())],
        ),
        (
            format!("/connections/{connection}/simplefin/reconnect"),
            vec![("setup_token", token.as_str())],
        ),
        (format!("/connections/{connection}/sync"), vec![]),
        (format!("/connections/{connection}/remove"), vec![]),
    ] {
        let missing = world.admin.post_form(&path, &fields).await;
        assert_eq!(missing.status, StatusCode::FORBIDDEN, "{path}");
        let mut wrong = fields.clone();
        wrong.push(("csrf", "AAAA"));
        let refused = world.admin.post_form(&path, &wrong).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        1
    );
    assert_eq!(world.status(connection).await, ConnectionStatus::Active);
    assert_eq!(world.run_count(connection).await, 0);
    assert_eq!(world.mock.requests().len(), 0);
}

#[tokio::test]
async fn member_cannot_remove_others_connection_via_page() {
    let world = world().await;
    let connection = world.seeded(world.alice).await;
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
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Family bank"));
    assert!(
        !page
            .body
            .contains(&format!("/connections/{connection}/remove"))
    );
    let refused = bob
        .post_form(
            &format!("/connections/{connection}/remove"),
            &[("csrf", &page.csrf())],
        )
        .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(world.status(connection).await, ConnectionStatus::Active);

    // Authorization comes before the form is checked: a blank token for
    // another member's connection is refused, not answered with a notice.
    let blank = bob
        .post_form(
            &format!("/connections/{connection}/simplefin/reconnect"),
            &[("csrf", &page.csrf()), ("setup_token", "")],
        )
        .await;
    assert_eq!(blank.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sync_form_reports_the_wait_when_rate_limited() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let csrf = world.admin.get("/connections").await.csrf();
    let path = format!("/connections/{connection}/sync");

    let queued = world.admin.post_form(&path, &[("csrf", &csrf)]).await;
    assert_eq!(queued.status, StatusCode::SEE_OTHER);
    world
        .script
        .bridge_replies
        .lock()
        .unwrap()
        .push_back(ACCOUNTS_FIRST.to_owned());
    world.run_queued(connection).await;

    let limited = world.admin.post_form(&path, &[("csrf", &csrf)]).await;

    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.header(header::RETRY_AFTER), "3600");
    assert!(limited.body.contains("role=\"alert\""));
    assert!(limited.body.contains("1 hour"));
    assert!(
        limited.body.contains("The provider allows another sync"),
        "the notice fits every provider"
    );
    assert_inline_free(&limited);
    assert_eq!(world.run_count(connection).await, 1);
}

#[tokio::test]
async fn reconnect_form_shown_and_used_for_reauth_required() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let mut tx = world.server.store.begin().await.unwrap();
    tx.update_connection_status(
        connection,
        coffer_store::ConnectionStatusChange {
            status: ConnectionStatus::ReauthRequired,
            last_error: Some("the provider refused the credentials".into()),
            attempted_at: None,
            succeeded_at: None,
            at: world.server.clock.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let page = world.admin.get("/connections").await;
    let action = format!("/connections/{connection}/simplefin/reconnect");
    assert!(page.body.contains(&format!("action=\"{action}\"")));
    world
        .script
        .claims
        .lock()
        .unwrap()
        .push_back(format!("http://{USER}:{PASSWORD}@{{host}}/simplefin"));
    let token = world.setup_token();
    let done = world
        .admin
        .post_form(&action, &[("csrf", &page.csrf()), ("setup_token", &token)])
        .await;

    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(world.status(connection).await, ConnectionStatus::Active);
}

#[tokio::test]
async fn remove_form_removes_own_connection() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    let csrf = world.admin.get("/connections").await.csrf();

    let removed = world
        .admin
        .post_form(
            &format!("/connections/{connection}/remove"),
            &[("csrf", &csrf)],
        )
        .await;

    assert_eq!(removed.status, StatusCode::SEE_OTHER);
    assert_eq!(world.status(connection).await, ConnectionStatus::Removed);
    let page = world.admin.get("/connections").await;
    assert!(
        !page
            .body
            .contains(&format!("/connections/{connection}/sync"))
    );
}
