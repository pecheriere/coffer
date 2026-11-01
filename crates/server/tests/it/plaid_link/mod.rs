//! The Plaid Link pages: the only page-scoped content security policy, a
//! public token that goes to Plaid and nowhere else, and update mode for a
//! connection that needs a new login. Plaid is a loopback mock.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod exchange;
mod page;
mod reconnect;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coffer_domain::identity::{Actor, Role};
use coffer_domain::ids::ConnectionId;
use coffer_domain::time::Clock as _;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_server::testkit::{MockUpstream, TEST_PUBLIC_URL, TestServer};
use coffer_store::{AuditFilter, ConnectionStatus, ConnectionStatusChange, PageRequest};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                           frame-ancestors 'none'; form-action 'self'; base-uri 'none'";
const LINK_CSP: &str = "default-src 'self'; \
                        script-src 'self' https://cdn.plaid.com/link/v2/stable/link-initialize.js; \
                        frame-src https://cdn.plaid.com; \
                        connect-src 'self' https://sandbox.plaid.com; \
                        style-src 'self' 'nonce-{nonce}'; img-src 'self'; \
                        frame-ancestors 'none'; form-action 'self'; base-uri 'none'";
const LINK_SCRIPT: &str = "https://cdn.plaid.com/link/v2/stable/link-initialize.js";

const NEW_PAGE: &str = "/connections/plaid/new";
const EXCHANGE_URL: &str = "/connections/plaid/exchange";
const LINK_TOKEN_CREATE: &str = "/link/token/create";
const PUBLIC_TOKEN_EXCHANGE: &str = "/item/public_token/exchange";

const ACCESS_MARKER: &str = "SECRET-ACCESS-71";
const ACCESS_TOKEN: &str = "access-sandbox-SECRET-ACCESS-71";
const PUBLIC_MARKER: &str = "PUBLICTOKEN-MARKER-72";
const PUBLIC_TOKEN: &str = "public-sandbox-PUBLICTOKEN-MARKER-72";
const LINK_MARKER: &str = "LINKTOKEN-MARKER-73";
const LINK_TOKEN: &str = "link-sandbox-LINKTOKEN-MARKER-73";
const CREDENTIALS: &str =
    r#"{"access_token":"access-sandbox-SECRET-ACCESS-71","item_id":"item-1"}"#;
const CURSOR: &str = "cursor-kept-74";

/// A status and a body to answer with.
type Answer = (StatusCode, String);

/// What the mock of Plaid answers by path, and what it was sent.
#[derive(Clone, Default)]
struct Script {
    replies: Arc<Mutex<HashMap<String, VecDeque<Answer>>>>,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Script {
    fn reply(&self, path: &str, status: StatusCode, body: impl Into<String>) {
        self.replies
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .push_back((status, body.into()));
    }

    fn link_token(&self) {
        self.reply(
            LINK_TOKEN_CREATE,
            StatusCode::OK,
            json!({ "link_token": LINK_TOKEN }).to_string(),
        );
    }

    fn seen(&self, path: &str) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| seen == path)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

async fn answer(State(script): State<Script>, uri: Uri, body: String) -> Response {
    let path = uri.path().to_owned();
    script
        .seen
        .lock()
        .unwrap()
        .push((path.clone(), serde_json::from_str(&body).unwrap()));
    let (status, reply) = script
        .replies
        .lock()
        .unwrap()
        .get_mut(&path)
        .and_then(VecDeque::pop_front)
        .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, "{}".to_owned()));
    (status, [(header::CONTENT_TYPE, "application/json")], reply).into_response()
}

struct World {
    server: TestServer,
    admin: Browser,
    alice: Actor,
    script: Script,
    _mock: MockUpstream,
}

/// Admin "alice" signed in, on a server configured for Plaid.
async fn world() -> World {
    let (server, signed_in, _) = with_admin().await;
    let script = Script::default();
    let mock = MockUpstream::start(Router::new().fallback(answer).with_state(script.clone())).await;
    let server = server.with_plaid(&mock);
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
        script,
        _mock: mock,
    }
}

impl World {
    async fn seeded(&self) -> ConnectionId {
        self.server
            .seed_connection(
                ProviderKind::Plaid,
                "Plaid bank",
                self.alice.member,
                CREDENTIALS,
            )
            .await
    }

    /// A Plaid connection that needs a new login and has a saved cursor.
    async fn needing_login(&self) -> ConnectionId {
        let id = self.seeded().await;
        let mut tx = self.server.store.begin().await.unwrap();
        tx.set_provider_state(id, Some(CURSOR.to_owned()))
            .await
            .unwrap();
        tx.update_connection_status(
            id,
            ConnectionStatusChange {
                status: ConnectionStatus::ReauthRequired,
                last_error: Some("Plaid needs a new login".to_owned()),
                attempted_at: None,
                succeeded_at: None,
                at: self.server.clock.now(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// Signed-in member "bob", with no connection of his own.
    async fn bob(&self) -> Browser {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let mut bob = Browser::new(&self.server);
        let mut key = self.server.authenticator();
        let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
        assert_eq!(done.status, StatusCode::OK);
        bob
    }

    /// The anti-forgery token of `browser`'s session.
    async fn csrf(browser: &mut Browser) -> String {
        browser.get("/connections").await.csrf()
    }

    async fn exchange(&mut self, body: &Value) -> Reply {
        let csrf = Self::csrf(&mut self.admin).await;
        self.admin
            .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), Some(&csrf), body)
            .await
    }

    async fn confirm(&mut self, id: ConnectionId) -> Reply {
        let csrf = Self::csrf(&mut self.admin).await;
        self.admin
            .post_json_with(
                &confirm_path(id),
                Some(TEST_PUBLIC_URL),
                Some(&csrf),
                &json!({}),
            )
            .await
    }

    async fn audit_text(&self) -> String {
        let events = self
            .server
            .store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap()
            .items;
        events
            .iter()
            .map(|event| {
                format!(
                    "{} {}",
                    event.action,
                    serde_json::to_string(&event.changes).unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn runs(&self, id: ConnectionId) -> Vec<coffer_store::SyncRunRecord> {
        self.server
            .store
            .list_sync_runs(id, &PageRequest::default())
            .await
            .unwrap()
            .items
    }

    /// Every file of the data directory, the database and its journal among
    /// them, as bytes.
    fn data_dir_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(&self.server.config.data_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                bytes.extend(std::fs::read(path).unwrap());
            }
        }
        bytes
    }
}

fn reconnect_path(id: ConnectionId) -> String {
    format!("/connections/{id}/plaid/reconnect")
}

fn confirm_path(id: ConnectionId) -> String {
    format!("/connections/{id}/plaid/reconnected")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
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

fn csp(reply: &Reply) -> &str {
    reply.header(header::CONTENT_SECURITY_POLICY)
}

/// The nonce in a Link page's policy.
fn policy_nonce(reply: &Reply) -> String {
    let (_, rest) = csp(reply).split_once("'nonce-").unwrap();
    rest.split('\'').next().unwrap().to_owned()
}
