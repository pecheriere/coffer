//! The web pages and ceremony endpoints, driven through the real router with
//! software passkeys and a hand-managed cookie.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use coffer_domain::identity::{Role, Scope, TokenName};
use coffer_server::auth::AuthError;
use coffer_server::http::router;
use coffer_server::testkit::{SoftAuthenticator, TEST_PUBLIC_URL, TestServer, link_token};
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use url::Url;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON body")
    }

    fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    /// The value of the hidden anti-forgery field.
    fn csrf(&self) -> String {
        self.between("name=\"csrf\" value=\"", "\"")
    }

    fn between(&self, start: &str, end: &str) -> String {
        let from = self
            .body
            .find(start)
            .unwrap_or_else(|| panic!("`{start}` not in page:\n{}", self.body))
            + start.len();
        let len = self.body[from..].find(end).expect("terminator");
        self.body[from..from + len].to_owned()
    }
}

/// A browser: the router and one cookie jar.
struct Browser {
    router: Router,
    cookie: Option<String>,
}

impl Browser {
    fn new(server: &TestServer) -> Self {
        Self {
            router: router(server.app_state()),
            cookie: None,
        }
    }

    async fn send(&mut self, request: Request<Body>) -> Reply {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("response");
        let (parts, body) = response.into_parts();
        let body = body.collect().await.expect("body").to_bytes();
        for value in parts.headers.get_all(header::SET_COOKIE) {
            let value = value.to_str().expect("ASCII cookie");
            if let Some(rest) = value.strip_prefix("coffer_session=") {
                let token = rest.split(';').next().unwrap_or_default();
                self.cookie = (!token.is_empty()).then(|| token.to_owned());
            }
        }
        Reply {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn builder(&self, method: Method, path: &str) -> axum::http::request::Builder {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(cookie) = &self.cookie {
            builder = builder.header(header::COOKIE, format!("coffer_session={cookie}"));
        }
        builder
    }

    async fn get(&mut self, path: &str) -> Reply {
        let request = self
            .builder(Method::GET, path)
            .body(Body::empty())
            .expect("request");
        self.send(request).await
    }

    /// The `Origin` header is set by hand. A real browser sends it on a form
    /// POST only because pages carry `Referrer-Policy: same-origin`; under
    /// `no-referrer` it would send `Origin: null` and the origin rule would
    /// refuse the form.
    async fn post_form(&mut self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let request = self
            .builder(Method::POST, path)
            .header(header::ORIGIN, TEST_PUBLIC_URL)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .expect("request");
        self.send(request).await
    }

    async fn post_form_from(&mut self, path: &str, origin: &str, fields: &[(&str, &str)]) -> Reply {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let request = self
            .builder(Method::POST, path)
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .expect("request");
        self.send(request).await
    }

    async fn post_json_with(
        &mut self,
        path: &str,
        origin: Option<&str>,
        csrf: Option<&str>,
        body: &Value,
    ) -> Reply {
        let mut builder = self
            .builder(Method::POST, path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("X-CSRF-Token", csrf);
        }
        let request = builder.body(Body::from(body.to_string())).expect("request");
        self.send(request).await
    }

    async fn post_json(&mut self, path: &str, body: &Value) -> Reply {
        self.post_json_with(path, Some(TEST_PUBLIC_URL), None, body)
            .await
    }

    /// Redeems an invite or setup link with `auth`: begin, register, finish.
    async fn enroll(
        &mut self,
        auth: &mut SoftAuthenticator,
        link: &Url,
        username: &str,
        display_name: &str,
    ) -> Reply {
        let begun = self
            .post_json(
                "/auth/enroll/begin",
                &json!({
                    "token": link_token(link),
                    "username": username,
                    "display_name": display_name,
                }),
            )
            .await;
        assert_eq!(begun.status, StatusCode::OK, "{}", begun.body);
        let begun = begun.json();
        let options: CreationChallengeResponse =
            serde_json::from_value(begun["options"].clone()).expect("options");
        let credential = auth.register(options).expect("register");
        self.post_json(
            "/auth/enroll/finish",
            &json!({
                "ceremony": begun["ceremony"],
                "label": "soft passkey",
                "credential": credential,
            }),
        )
        .await
    }

    async fn login(&mut self, auth: &mut SoftAuthenticator, username: &str) -> Reply {
        let begun = self
            .post_json("/auth/login/begin", &json!({ "username": username }))
            .await;
        assert_eq!(begun.status, StatusCode::OK, "{}", begun.body);
        let begun = begun.json();
        let options: RequestChallengeResponse =
            serde_json::from_value(begun["options"].clone()).expect("options");
        let credential = auth.sign(options);
        self.post_json(
            "/auth/login/finish",
            &json!({ "ceremony": begun["ceremony"], "credential": credential }),
        )
        .await
    }
}

/// A server whose admin "alice" is enrolled and signed in over HTTP.
async fn with_admin() -> (TestServer, Browser, SoftAuthenticator) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);
    let mut auth = server.authenticator();
    let done = browser.enroll(&mut auth, &link, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    (server, browser, auth)
}

#[tokio::test]
async fn full_journey() {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    assert!(setup.path().starts_with("/setup/"));

    // The bootstrap link renders a form and enrolls the first admin.
    let mut admin = Browser::new(&server);
    let page = admin.get(setup.path()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("name=\"username\""));
    let mut alice_key = server.authenticator();
    let done = admin.enroll(&mut alice_key, &setup, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(done.json()["redirect"], "/account");
    assert!(done.header(header::SET_COOKIE).contains("HttpOnly"));

    let account = admin.get("/account").await;
    assert_eq!(account.status, StatusCode::OK);
    assert!(account.body.contains("Alice"));

    // The admin invites bob through the form and the link is shown once.
    let csrf = account.csrf();
    let invites = admin.get("/admin/invites").await;
    assert_eq!(invites.status, StatusCode::OK);
    let created = admin
        .post_form(
            "/admin/invites",
            &[("csrf", &csrf), ("role", "member"), ("display_name", "Bob")],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.header(header::CACHE_CONTROL), "no-store");
    let link = Url::parse(&created.between("id=\"new-link\">", "<")).expect("invite link");
    assert!(link.path().starts_with("/invite/"));

    // Bob enrolls with his own authenticator and cannot see admin pages.
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    let done = bob.enroll(&mut bob_key, &link, "bob", "Bob").await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let denied = bob.get("/admin/members").await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert!(denied.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(
        bob.get("/admin/invites").await.status,
        StatusCode::FORBIDDEN
    );

    // Bob's token form offers no scope above his role's ceiling, and the
    // server refuses one anyway.
    let bob_account = bob.get("/account").await;
    assert!(bob_account.body.contains("value=\"write\""));
    assert!(!bob_account.body.contains("value=\"admin\""));
    let refused = bob
        .post_form(
            "/account/tokens",
            &[
                ("csrf", &bob_account.csrf()),
                ("name", "x"),
                ("scope", "admin"),
            ],
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    // The admin sees bob in the members list and creates a write token.
    let members = admin.get("/admin/members").await;
    assert_eq!(members.status, StatusCode::OK);
    assert!(members.body.contains("bob"));
    let created = admin
        .post_form(
            "/account/tokens",
            &[
                ("csrf", &csrf),
                ("name", "planner"),
                ("scope", "write"),
                ("expires_days", "30"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.header(header::CACHE_CONTROL), "no-store");
    let token = created.between("id=\"new-token\">", "<");
    assert!(token.starts_with("coffer_"));

    // The token is shown once: a reload lists the token without its secret.
    let reload = admin.get("/account").await;
    assert!(reload.body.contains("planner"));
    assert!(!reload.body.contains(&token));

    // And it works as a bearer token on the API.
    let request = Request::builder()
        .uri("/api/v1/me")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let me = admin.send(request).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["agent"]["scope"], "write");

    // Signing in again over the login endpoints works, and so does logout.
    let mut returning = Browser::new(&server);
    let done = returning.login(&mut alice_key, "alice").await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(done.json()["redirect"], "/account");
    let page = returning.get("/account").await;
    assert_eq!(page.status, StatusCode::OK);
    let out = returning
        .post_form("/logout", &[("csrf", &page.csrf())])
        .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(out.header(header::LOCATION), "/login");
    let after = returning.get("/account").await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
    assert_eq!(after.header(header::LOCATION), "/login");
}

#[tokio::test]
async fn post_without_csrf_rejected() {
    let (_server, mut admin, _) = with_admin().await;

    let missing = admin
        .post_form("/account/tokens", &[("name", "sneaky"), ("scope", "read")])
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let wrong = admin
        .post_form(
            "/account/tokens",
            &[("csrf", "AAAA"), ("name", "sneaky"), ("scope", "read")],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let logout = admin.post_form("/logout", &[]).await;
    assert_eq!(logout.status, StatusCode::FORBIDDEN);
    let invite = admin
        .post_form("/admin/invites", &[("role", "member")])
        .await;
    assert_eq!(invite.status, StatusCode::FORBIDDEN);

    // The add-passkey ceremony needs the header.
    let begin = admin
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            None,
            &json!({}),
        )
        .await;
    assert_eq!(begin.status, StatusCode::FORBIDDEN);

    // Nothing happened, and the session is still good.
    let after = admin.get("/account").await;
    assert_eq!(after.status, StatusCode::OK);
    assert!(!after.body.contains("sneaky"));
}

#[tokio::test]
async fn security_headers_present() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let page = browser.get("/login").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.header(header::CONTENT_SECURITY_POLICY),
        "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
         form-action 'self'; base-uri 'none'"
    );
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
    assert_eq!(page.header(header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert!(
        !page.body.contains("<script>") && !page.body.contains("style=\""),
        "pages carry no inline script or style"
    );
}

/// `same-origin`, not `no-referrer`: under `no-referrer` a browser sends
/// `Origin: null` on a form POST, which the origin rule refuses, so every
/// plain form would fail. `same-origin` still sends nothing to other sites,
/// so setup and invite tokens in the path never leave coffer.
#[tokio::test]
async fn referrer_policy_lets_forms_send_their_origin() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    for path in ["/login", "/static/coffer.css"] {
        let reply = browser.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(
            reply.header(header::REFERRER_POLICY),
            "same-origin",
            "{path}"
        );
    }
}

#[tokio::test]
async fn invite_page_no_store() {
    let server = TestServer::new().await;
    let setup = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);

    let page = browser.get(setup.path()).await;

    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert_eq!(page.header(header::REFERRER_POLICY), "same-origin");
}

#[tokio::test]
async fn unusable_link_page_is_generic() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let page = browser.get("/invite/not-a-real-token").await;

    assert_eq!(page.status, StatusCode::GONE);
    assert_eq!(page.header(header::CACHE_CONTROL), "no-store");
    assert!(!page.body.contains("not-a-real-token"));
}

#[tokio::test]
async fn cross_origin_post_rejected() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let body = json!({ "username": "alice" });

    let missing = browser
        .post_json_with("/auth/login/begin", None, None, &body)
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let foreign = browser
        .post_json_with("/auth/login/begin", Some("https://evil.test"), None, &body)
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    let enroll = browser
        .post_json_with(
            "/auth/enroll/begin",
            Some("https://evil.test"),
            None,
            &json!({ "token": "x" }),
        )
        .await;
    assert_eq!(enroll.status, StatusCode::FORBIDDEN);

    // The same request from the public origin reaches the handler.
    let ok = browser
        .post_json_with("/auth/login/begin", Some(TEST_PUBLIC_URL), None, &body)
        .await;
    assert_ne!(ok.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bearer_token_does_not_authenticate_pages() {
    let (server, admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("agent").unwrap(),
            Scope::Admin,
            None,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", secrecy::ExposeSecret::expose_secret(&token));
    let mut anonymous = Browser::new(&server);

    for path in ["/account", "/admin/members", "/admin/invites"] {
        let request = Request::builder()
            .uri(path)
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap();
        let reply = anonymous.send(request).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(reply.header(header::LOCATION), "/login", "{path}");
    }
    // The cookie-less browser cannot use the ceremony endpoints either.
    let reply = anonymous
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            None,
            &json!({}),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn passkeys_can_be_added_and_removed() {
    let (server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    let csrf = account.csrf();

    // Add a second passkey through the session-bound ceremony.
    let begun = admin
        .post_json_with(
            "/auth/passkeys/begin",
            Some(TEST_PUBLIC_URL),
            Some(&csrf),
            &json!({}),
        )
        .await;
    assert_eq!(begun.status, StatusCode::OK, "{}", begun.body);
    let begun = begun.json();
    let options: CreationChallengeResponse =
        serde_json::from_value(begun["options"].clone()).unwrap();
    let mut second = server.authenticator();
    let credential = second.register(options).unwrap();
    let done = admin
        .post_json_with(
            "/auth/passkeys/finish",
            Some(TEST_PUBLIC_URL),
            Some(&csrf),
            &json!({
                "ceremony": begun["ceremony"],
                "label": "second key",
                "credential": credential,
            }),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);

    let listed = admin.get("/account").await;
    assert!(listed.body.contains("second key"));

    // Remove it again; the form carries the passkey id.
    let after_label = Reply {
        status: listed.status,
        headers: HeaderMap::new(),
        body: listed.body[listed.body.find("second key").unwrap()..].to_owned(),
    };
    let id = after_label.between("action=\"/account/passkeys/", "/remove\"");
    let removed = admin
        .post_form(
            &format!("/account/passkeys/{id}/remove"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::SEE_OTHER);
    assert!(!admin.get("/account").await.body.contains("second key"));
}

#[tokio::test]
async fn last_passkey_cannot_be_removed() {
    let (_server, mut admin, _) = with_admin().await;
    let account = admin.get("/account").await;
    let id = account.between("action=\"/account/passkeys/", "/remove\"");

    let reply = admin
        .post_form(
            &format!("/account/passkeys/{id}/remove"),
            &[("csrf", &account.csrf())],
        )
        .await;

    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body.contains("last passkey"));
    assert!(admin.get("/account").await.body.contains("soft passkey"));
}

#[tokio::test]
async fn members_page_manages_members_and_reenroll_links() {
    let (server, mut admin, _) = with_admin().await;
    let invite = server
        .identity
        .create_invite(
            &server
                .sessions
                .resolve(admin.cookie.as_deref().unwrap())
                .await
                .unwrap(),
            Role::Member,
            None,
        )
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut bob_key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );

    let members = admin.get("/admin/members").await;
    let csrf = members.csrf();
    let bob_row = Reply {
        status: members.status,
        headers: HeaderMap::new(),
        body: members.body[members.body.find("<td>bob</td>").unwrap()..].to_owned(),
    };
    let bob_id = bob_row.between("action=\"/admin/members/", "/role\"");

    // A re-enroll link is issued and shown once; its page hides the name
    // fields because the member already exists.
    let issued = admin
        .post_form(
            &format!("/admin/members/{bob_id}/reenroll"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    assert_eq!(issued.header(header::CACHE_CONTROL), "no-store");
    let link = Url::parse(&issued.between("id=\"new-link\">", "<")).unwrap();
    let page = Browser::new(&server).get(link.path()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.body.contains("name=\"username\""));

    // Promote and disable through the forms; disabling ends bob's session.
    let promoted = admin
        .post_form(
            &format!("/admin/members/{bob_id}/role"),
            &[("csrf", &csrf), ("role", "admin")],
        )
        .await;
    assert_eq!(promoted.status, StatusCode::SEE_OTHER);
    assert_eq!(bob.get("/admin/members").await.status, StatusCode::OK);
    let disabled = admin
        .post_form(
            &format!("/admin/members/{bob_id}/disable"),
            &[("csrf", &csrf)],
        )
        .await;
    assert_eq!(disabled.status, StatusCode::SEE_OTHER);
    assert_eq!(bob.get("/account").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn invites_page_lists_and_revokes_pending_invites() {
    let (_server, mut admin, _) = with_admin().await;
    let page = admin.get("/admin/invites").await;
    let csrf = page.csrf();
    admin
        .post_form("/admin/invites", &[("csrf", &csrf), ("role", "member")])
        .await;

    let listed = admin.get("/admin/invites").await;
    let id = listed.between("action=\"/admin/invites/", "/revoke\"");
    let revoked = admin
        .post_form(&format!("/admin/invites/{id}/revoke"), &[("csrf", &csrf)])
        .await;

    assert_eq!(revoked.status, StatusCode::SEE_OTHER);
    assert!(!admin.get("/admin/invites").await.body.contains("/revoke\""));
}

#[tokio::test]
async fn static_assets_are_served() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);

    let script = browser.get("/static/coffer.js").await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(
        script
            .header(header::CONTENT_TYPE)
            .starts_with("text/javascript")
    );
    assert!(script.body.contains("navigator.credentials"));
    let style = browser.get("/static/coffer.css").await;
    assert_eq!(style.status, StatusCode::OK);
    assert!(style.header(header::CONTENT_TYPE).starts_with("text/css"));
}

#[tokio::test]
async fn only_the_exact_public_origin_may_post() {
    let (_server, mut admin, _) = with_admin().await;
    let csrf = admin.get("/account").await.csrf();
    let fields = [
        ("csrf", csrf.as_str()),
        ("name", "sneaky"),
        ("scope", "read"),
    ];

    // A logged-in form post with a valid CSRF token is still refused from
    // another origin, as an HTML error page.
    for origin in [
        "https://evil.test",
        "null",
        "http://coffer.test",
        "https://coffer.test:8443",
        "https://coffer.test.evil.test",
    ] {
        let reply = admin
            .post_form_from("/account/tokens", origin, &fields)
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{origin}");
        assert!(
            reply.header(header::CONTENT_TYPE).starts_with("text/html"),
            "{origin}"
        );
        let json = admin
            .post_json_with(
                "/auth/login/begin",
                Some(origin),
                None,
                &json!({"username": "alice"}),
            )
            .await;
        assert_eq!(json.status, StatusCode::FORBIDDEN, "{origin}");
    }
    assert!(!admin.get("/account").await.body.contains("sneaky"));
}

#[tokio::test]
async fn unreadable_bodies_get_generic_errors() {
    let (_server, mut admin, _) = with_admin().await;
    let csrf = admin.get("/account").await.csrf();

    // Page route: a form missing its fields is a generic HTML error.
    let form = admin.post_form("/account/tokens", &[("csrf", &csrf)]).await;
    assert_eq!(form.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(form.header(header::CONTENT_TYPE).starts_with("text/html"));
    assert!(!form.body.contains("missing field"));

    // Ceremony route: missing and malformed JSON are 422 `invalid`.
    for body in [json!({}), json!("not an object")] {
        let reply = admin.post_json("/auth/login/begin", &body).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(reply.json()["code"], "invalid");
        assert!(!reply.body.contains("missing field"));
        assert!(!reply.body.contains("invalid type"));
    }
    let request = Request::builder()
        .method(Method::POST)
        .uri("/auth/login/begin")
        .header(header::ORIGIN, TEST_PUBLIC_URL)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{"))
        .unwrap();
    let malformed = admin.send(request).await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(malformed.json()["code"], "invalid");
}

#[tokio::test]
async fn signing_in_again_ends_the_previous_session() {
    let (server, mut admin, mut auth) = with_admin().await;
    let old = admin.cookie.clone().unwrap();

    let done = admin.login(&mut auth, "alice").await;

    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let new = admin.cookie.clone().unwrap();
    assert_ne!(old, new);
    assert!(matches!(
        server.sessions.resolve(&old).await,
        Err(AuthError::Unauthenticated)
    ));
    server.sessions.resolve(&new).await.unwrap();
}

#[tokio::test]
async fn admin_sees_and_revokes_member_tokens() {
    let (server, mut admin, _) = with_admin().await;
    let alice = server
        .sessions
        .resolve(admin.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let invite = server
        .identity
        .create_invite(&alice, Role::Member, None)
        .await
        .unwrap();
    let mut bob = Browser::new(&server);
    let mut bob_key = server.authenticator();
    assert_eq!(
        bob.enroll(&mut bob_key, &invite, "bob", "Bob").await.status,
        StatusCode::OK
    );
    let bob_actor = server
        .sessions
        .resolve(bob.cookie.as_deref().unwrap())
        .await
        .unwrap();
    let (bob_token, bob_secret) = server
        .agent_tokens
        .create(
            &bob_actor,
            TokenName::parse("bob-planner").unwrap(),
            Scope::Read,
            None,
        )
        .await
        .unwrap();
    let (alice_token, alice_secret) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("alice-agent").unwrap(),
            Scope::Read,
            None,
        )
        .await
        .unwrap();

    // The members page lists every member's tokens, without their secrets.
    let members = admin.get("/admin/members").await;
    assert_eq!(members.status, StatusCode::OK);
    assert!(members.body.contains("bob-planner"));
    assert!(members.body.contains(&bob_token.prefix));
    assert!(!members.body.contains(bob_secret.expose_secret()));
    let revoke_path = format!("/admin/tokens/{}/revoke", bob_token.id);
    assert!(members.body.contains(&revoke_path), "{}", members.body);

    // A member may not use it, not even on another member's token.
    let bob_page = bob.get("/account").await;
    let denied = bob
        .post_form(
            &format!("/admin/tokens/{}/revoke", alice_token.id),
            &[("csrf", &bob_page.csrf())],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bearer_status(&mut bob, alice_secret.expose_secret()).await,
        StatusCode::OK
    );

    // Without the anti-forgery token, nothing happens.
    let forged = admin.post_form(&revoke_path, &[]).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bearer_status(&mut admin, bob_secret.expose_secret()).await,
        StatusCode::OK
    );

    let revoked = admin
        .post_form(&revoke_path, &[("csrf", &members.csrf())])
        .await;
    assert_eq!(revoked.status, StatusCode::SEE_OTHER);
    assert_eq!(revoked.header(header::LOCATION), "/admin/members");
    assert_eq!(
        bearer_status(&mut admin, bob_secret.expose_secret()).await,
        StatusCode::UNAUTHORIZED
    );
    let listed = admin.get("/admin/members").await;
    assert!(!listed.body.contains(&revoke_path));
    assert!(listed.body.contains("revoked"));
}

/// Status of `GET /api/v1/me` with `secret` as the bearer token.
async fn bearer_status(browser: &mut Browser, secret: &str) -> StatusCode {
    let request = Request::builder()
        .uri("/api/v1/me")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .body(Body::empty())
        .unwrap();
    browser.send(request).await.status
}

#[tokio::test]
async fn sign_in_trims_the_username() {
    let (server, _, mut auth) = with_admin().await;
    let mut browser = Browser::new(&server);

    let done = browser.login(&mut auth, "  Alice ").await;

    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
}

#[tokio::test]
async fn unverifiable_registration_is_a_client_error() {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let mut browser = Browser::new(&server);
    let begin = json!({
        "token": link_token(&link),
        "username": "alice",
        "display_name": "Alice",
    });
    let first = browser.post_json("/auth/enroll/begin", &begin).await.json();
    let second = browser.post_json("/auth/enroll/begin", &begin).await.json();
    let options: CreationChallengeResponse =
        serde_json::from_value(first["options"].clone()).unwrap();
    let credential = server.authenticator().register(options).unwrap();

    // Answers the first ceremony's challenge but finishes the second one.
    let rejected = browser
        .post_json(
            "/auth/enroll/finish",
            &json!({
                "ceremony": second["ceremony"],
                "label": "soft passkey",
                "credential": credential,
            }),
        )
        .await;

    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.body
    );
    assert_eq!(rejected.json()["code"], "passkey_rejected");
    assert_eq!(server.store.list_members().await.unwrap().len(), 0);
}

/// `GET` and `HEAD` of `path` with the browser's cookie, if any.
async fn get_and_head(browser: &mut Browser, path: &str) -> [Reply; 2] {
    let get = browser.get(path).await;
    let request = browser
        .builder(Method::HEAD, path)
        .body(Body::empty())
        .unwrap();
    let head = browser.send(request).await;
    [get, head]
}

#[tokio::test]
async fn root_sends_a_visitor_without_a_session_to_sign_in() {
    let server = TestServer::new().await;
    let mut browser = Browser::new(&server);
    let mut stale = Browser::new(&server);
    stale.cookie = Some("not-a-session".to_owned());

    for reply in get_and_head(&mut browser, "/")
        .await
        .into_iter()
        .chain(get_and_head(&mut stale, "/").await)
    {
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        assert_eq!(reply.header(header::LOCATION), "/login");
    }
}

#[tokio::test]
async fn root_sends_a_signed_in_member_home() {
    let (_server, mut admin, _) = with_admin().await;

    for reply in get_and_head(&mut admin, "/").await {
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        assert_eq!(reply.header(header::LOCATION), "/account");
    }
}
