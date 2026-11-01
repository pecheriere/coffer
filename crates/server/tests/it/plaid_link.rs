//! The Plaid Link pages: the only page-scoped content security policy, a
//! public token that goes to Plaid and nowhere else, and update mode for a
//! connection that needs a new login. Plaid is a loopback mock.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_domain::sync::SyncTrigger;
use coffer_domain::time::Clock as _;
use coffer_server::assets;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::ProviderKind;
use coffer_server::testkit::browser::{Browser, Reply, with_admin};
use coffer_server::testkit::{
    MockUpstream, TEST_PUBLIC_URL, TestServer, captured_log_text, log_marker,
};
use coffer_store::{AuditFilter, ConnectionStatus, ConnectionStatusChange, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};

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

#[tokio::test]
async fn plaid_pages_absent_without_config() {
    let (server, mut admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let id = server
        .seed_connection(ProviderKind::Plaid, "Old", alice.member, CREDENTIALS)
        .await;
    let csrf = admin.get("/connections").await.csrf();

    assert_eq!(admin.get(NEW_PAGE).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        admin.get(&reconnect_path(id)).await.status,
        StatusCode::NOT_FOUND
    );
    for path in [EXCHANGE_URL.to_owned(), confirm_path(id)] {
        let reply = admin
            .post_json_with(&path, Some(TEST_PUBLIC_URL), Some(&csrf), &json!({}))
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    let page = admin.get("/connections").await;
    assert!(
        !page.body.contains("/plaid/"),
        "no Plaid link without Plaid configured"
    );
    assert!(!page.body.contains("cdn.plaid.com"));
}

#[tokio::test]
async fn link_page_has_plaid_csp_only_there() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.script.link_token();
    world.script.link_token();

    for path in [NEW_PAGE.to_owned(), reconnect_path(id)] {
        let page = world.admin.get(&path).await;
        assert_eq!(page.status, StatusCode::OK);
        let expected = LINK_CSP.replace("{nonce}", &policy_nonce(&page));
        assert_eq!(csp(&page), expected);
        assert!(!csp(&page).contains("unsafe-"), "never an unsafe source");
    }

    for path in [
        "/connections",
        "/account",
        "/login",
        "/healthz",
        "/api/v1/me",
        "/no-such-page",
        assets::CSS_BASE_CSS.url,
        assets::JS_PLAID_LINK_JS.url,
    ] {
        let reply = world.admin.get(path).await;
        assert_eq!(csp(&reply), DEFAULT_CSP, "every other route: {path}");
    }

    // The endpoints the page calls keep the default too, in success and
    // refusal, and the same pages on a server without Plaid.
    for path in ["/connections", "/account", "/login"] {
        let reply = world.admin.get(path).await;
        assert!(!reply.body.contains("nonce"), "no other page has a nonce");
    }
    let refused = world.exchange(&json!({})).await;
    assert_eq!(csp(&refused), DEFAULT_CSP);
    let confirmed = world.confirm(id).await;
    assert_eq!(confirmed.status, StatusCode::OK);
    assert_eq!(csp(&confirmed), DEFAULT_CSP);

    let (_, mut plain, _) = with_admin().await;
    for path in ["/connections", "/account", NEW_PAGE] {
        assert_eq!(csp(&plain.get(path).await), DEFAULT_CSP);
    }
    let mut anonymous = Browser::new(&TestServer::new().await);
    assert_eq!(csp(&anonymous.get("/login").await), DEFAULT_CSP);
}

#[tokio::test]
async fn link_page_failure_keeps_default_csp() {
    let mut world = world().await;
    world.script.reply(
        LINK_TOKEN_CREATE,
        StatusCode::BAD_REQUEST,
        json!({ "error_type": "INVALID_REQUEST", "error_code": "MISSING_FIELDS" }).to_string(),
    );

    let page = world.admin.get(NEW_PAGE).await;

    assert_eq!(page.status, StatusCode::BAD_GATEWAY);
    assert_eq!(csp(&page), DEFAULT_CSP);
    assert!(!page.body.contains(LINK_SCRIPT), "no third-party script");
    assert!(!page.body.contains("data-link-token"));
    assert!(!page.body.contains("nonce"), "no nonce on a failed page");
}

/// Plaid's script reads its own tag's nonce for the stylesheet it adds, so the
/// tag and the policy must agree, and no other element may hold the nonce.
#[tokio::test]
async fn nonce_is_on_plaids_script_tag_only() {
    let mut world = world().await;
    let id = world.needing_login().await;
    world.script.link_token();
    world.script.link_token();

    for path in [NEW_PAGE.to_owned(), reconnect_path(id)] {
        let page = world.admin.get(&path).await;
        let nonce = policy_nonce(&page);
        let scripts = tags(&page.body, "script");
        let plaid: Vec<&&str> = scripts
            .iter()
            .filter(|script| script.contains(LINK_SCRIPT))
            .collect();
        assert_eq!(plaid.len(), 1, "one Plaid script");
        assert!(
            plaid[0].contains(&format!(" nonce=\"{nonce}\"")),
            "the tag's nonce is the policy's"
        );
        assert_eq!(
            page.body.matches("nonce=").count(),
            1,
            "one nonce attribute"
        );
        assert_eq!(
            page.body.matches(&nonce).count(),
            1,
            "the value appears once"
        );
    }
}

#[tokio::test]
async fn every_render_has_a_fresh_nonce_and_logs_none() {
    let mut world = world().await;
    world.script.link_token();
    world.script.link_token();
    let marker = log_marker();

    let first = policy_nonce(&world.admin.get(NEW_PAGE).await);
    let second = policy_nonce(&world.admin.get(NEW_PAGE).await);

    assert!(first != second, "two renders never share a nonce");
    let bytes = URL_SAFE_NO_PAD.decode(&first).unwrap();
    assert!(bytes.len() >= 16, "at least 128 bits");
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    for nonce in [&first, &second] {
        assert!(!log.contains(nonce.as_str()), "a nonce is never logged");
    }
}

#[tokio::test]
async fn link_page_has_no_inline_script() {
    let mut world = world().await;
    world.script.link_token();

    let page = world.admin.get(NEW_PAGE).await;

    let scripts = tags(&page.body, "script");
    assert!(
        scripts.iter().all(|script| script.contains(" src=\"")),
        "every script has a src"
    );
    for src in [
        assets::JS_COFFER_JS.url,
        LINK_SCRIPT,
        assets::JS_PLAID_LINK_JS.url,
    ] {
        let tag = format!(" src=\"{src}\"");
        assert!(
            scripts.iter().any(|script| script.contains(&tag)),
            "a script is missing from the page"
        );
    }
    assert_eq!(scripts.len(), 3, "no other script");
    assert!(!page.body.contains("style="), "no inline style");
    assert_eq!(tags(&page.body, "style").len(), 0);
    for handler in ["onclick", "onload", "onerror"] {
        assert!(!page.body.contains(handler), "no inline event handler");
    }
    assert!(page.body.contains("aria-current=\"page\">Connections<"));

    let script = world.admin.get(assets::JS_PLAID_LINK_JS.url).await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(
        script
            .header(header::CONTENT_TYPE)
            .starts_with("text/javascript")
    );
}

#[tokio::test]
async fn link_token_is_in_the_page_and_no_log() {
    let mut world = world().await;
    world.script.link_token();
    let marker = log_marker();

    let page = world.admin.get(NEW_PAGE).await;

    let attribute = format!("data-link-token=\"{LINK_TOKEN}\"");
    assert!(page.body.contains(&attribute), "the token is in the page");
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    assert!(!log.contains(LINK_MARKER), "the token must not be logged");
    assert!(
        !world.audit_text().await.contains(LINK_MARKER),
        "the token must not be audited"
    );
}

#[tokio::test]
async fn new_page_asks_for_a_new_item() {
    let mut world = world().await;
    world.script.link_token();

    let page = world.admin.get(NEW_PAGE).await;

    assert!(page.body.contains("Connect with Plaid"));
    assert!(page.body.contains("data-csrf=\""));
    assert!(
        page.body
            .contains(&format!("data-submit=\"{EXCHANGE_URL}\""))
    );
    assert!(
        tags(&page.body, "input")
            .iter()
            .any(|input| input.contains("plaid-label"))
    );
    let seen = world.script.seen(LINK_TOKEN_CREATE);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["products"], json!(["transactions"]));
    assert!(seen[0].get("access_token").is_none());
    assert_eq!(
        seen[0]["user"]["client_user_id"],
        world.alice.member.to_string()
    );
}

#[tokio::test]
async fn exchange_requires_session_and_csrf() {
    let mut world = world().await;
    let body = json!({ "label": "Joint", "public_token": PUBLIC_TOKEN });
    let csrf = World::csrf(&mut world.admin).await;

    // A bearer token authenticates no page endpoint.
    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("agent").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut anonymous = Browser::new(&world.server);
    let bearer = Request::builder()
        .method(Method::POST)
        .uri(EXCHANGE_URL)
        .header(header::ORIGIN, TEST_PUBLIC_URL)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        )
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        anonymous.send(bearer).await.status,
        StatusCode::UNAUTHORIZED
    );

    let missing = world
        .admin
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), None, &body)
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let wrong = world
        .admin
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), Some("wrong"), &body)
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let cross = world
        .admin
        .post_json_with(
            EXCHANGE_URL,
            Some("https://evil.example"),
            Some(&csrf),
            &body,
        )
        .await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    let no_origin = world
        .admin
        .post_json_with(EXCHANGE_URL, None, Some(&csrf), &body)
        .await;
    assert_eq!(no_origin.status, StatusCode::FORBIDDEN);

    // Authentication comes first: a body it cannot read says nothing to a
    // request without a session.
    let unreadable = anonymous
        .post_json_with(EXCHANGE_URL, Some(TEST_PUBLIC_URL), None, &json!(7))
        .await;
    assert_eq!(unreadable.status, StatusCode::UNAUTHORIZED);

    assert_eq!(
        world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(),
        0,
        "nothing reached Plaid"
    );
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        0
    );
}

#[tokio::test]
async fn exchange_stores_sealed_access_token() {
    let mut world = world().await;
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::OK,
        json!({ "access_token": ACCESS_TOKEN, "item_id": "item-1" }).to_string(),
    );
    let marker = log_marker();

    let reply = world
        .exchange(&json!({ "label": "Joint", "public_token": PUBLIC_TOKEN }))
        .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "redirect": "/connections" }));
    let seen = world.script.seen(PUBLIC_TOKEN_EXCHANGE);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["public_token"], PUBLIC_TOKEN);

    let connections = world.server.store.list_connections().await.unwrap();
    assert_eq!(connections.len(), 1);
    let connection = &connections[0];
    assert_eq!(connection.provider, "plaid");
    assert_eq!(connection.label.as_str(), "Joint");
    assert_eq!(connection.owner, world.alice.member);
    assert_eq!(connection.status, ConnectionStatus::Active);
    let opened = world
        .server
        .vault
        .open(
            connection.id,
            &connection.credentials.clone().unwrap().into(),
        )
        .unwrap();
    assert!(
        opened.expose_secret().contains(ACCESS_MARKER),
        "positive control: the sealed bytes hold the token"
    );

    for marker in [ACCESS_MARKER, PUBLIC_MARKER] {
        assert!(
            !contains(&world.data_dir_bytes(), marker),
            "no token in the data directory"
        );
        assert!(!world.audit_text().await.contains(marker), "nor the audit");
        assert!(!reply.body.contains(marker), "nor the response");
        assert!(!captured_log_text().contains(marker), "nor the log");
    }
    assert!(captured_log_text().contains(&marker));

    let runs = world.runs(connection.id).await;
    assert_eq!(runs.len(), 1, "a sync is queued");
    assert_eq!(runs[0].trigger, SyncTrigger::Initial);
}

#[tokio::test]
async fn public_token_never_echoed_on_error() {
    let mut world = world().await;
    let marker = log_marker();
    let body = json!({ "label": "Joint", "public_token": PUBLIC_TOKEN });
    let echoing = json!({
        "error_type": "INVALID_INPUT",
        "error_code": "INVALID_PUBLIC_TOKEN",
        "error_message": format!("the token {PUBLIC_TOKEN} is no good"),
        "display_message": PUBLIC_TOKEN,
    });
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::BAD_REQUEST,
        echoing.to_string(),
    );
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::BAD_GATEWAY,
        format!("not json {PUBLIC_TOKEN}"),
    );
    world.script.reply(
        PUBLIC_TOKEN_EXCHANGE,
        StatusCode::OK,
        format!("{{\"unexpected\":\"{PUBLIC_TOKEN}\"}}"),
    );

    for status in [
        StatusCode::BAD_GATEWAY,
        StatusCode::BAD_GATEWAY,
        StatusCode::BAD_GATEWAY,
    ] {
        let reply = world.exchange(&body).await;
        assert_eq!(reply.status, status);
        assert!(!reply.body.contains(PUBLIC_MARKER), "a Plaid failure");
    }
    assert_eq!(world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(), 3);

    // Refused by coffer, with no request to Plaid.
    for refused in [
        json!({ "label": "", "public_token": PUBLIC_TOKEN }),
        json!({ "label": "a\u{7}b", "public_token": PUBLIC_TOKEN }),
        json!({ "label": "x".repeat(101), "public_token": PUBLIC_TOKEN }),
        json!({ "label": "Joint", "public_token": "   " }),
        json!({ "public_token": PUBLIC_TOKEN }),
        json!({ "label": 7, "public_token": PUBLIC_TOKEN }),
        json!({ "label": "Joint", "public_token": PUBLIC_TOKEN, "extra": PUBLIC_TOKEN }),
    ] {
        let reply = world.exchange(&refused).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!reply.body.contains(PUBLIC_MARKER), "a refusal");
    }
    assert_eq!(world.script.seen(PUBLIC_TOKEN_EXCHANGE).len(), 3);
    assert_eq!(
        world.server.store.list_connections().await.unwrap().len(),
        0
    );

    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds the marker");
    assert!(!log.contains(PUBLIC_MARKER), "the token must not be logged");
    assert!(!world.audit_text().await.contains(PUBLIC_MARKER));
}

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

#[tokio::test]
async fn connections_page_offers_plaid_when_configured() {
    let mut world = world().await;
    let needing = world.needing_login().await;
    let healthy = world.seeded().await;

    let page = world.admin.get("/connections").await;

    assert_eq!(csp(&page), DEFAULT_CSP);
    assert!(page.body.contains(&format!("href=\"{NEW_PAGE}\"")));
    assert!(
        page.body
            .contains(&format!("href=\"{}\"", reconnect_path(needing)))
    );
    assert!(
        !page.body.contains(&reconnect_path(healthy)),
        "only a connection that needs it"
    );
    assert!(
        !page
            .body
            .contains(&format!("/connections/{needing}/simplefin/reconnect")),
        "a Plaid connection is not offered the SimpleFin form"
    );
    assert!(!page.body.contains("cdn.plaid.com"));
}
