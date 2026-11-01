//! A browser for the test server: the real router and one cookie jar.

#![expect(
    clippy::expect_used,
    reason = "test harness: a setup step that fails must abort the test that called it"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use url::Url;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

use super::{SoftAuthenticator, TEST_PUBLIC_URL, TestServer, link_token};
use crate::http::router;

/// A response, read to the end.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    /// The body as text; invalid UTF-8 is replaced.
    pub body: String,
    /// The body as received.
    pub bytes: Vec<u8>,
}

impl Reply {
    /// The body parsed as JSON.
    ///
    /// # Panics
    ///
    /// If the body is not JSON.
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON body")
    }

    /// The value of a response header, or empty if it is missing or not text.
    #[must_use]
    pub fn header(&self, name: header::HeaderName) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    /// The value of the hidden anti-forgery field.
    #[must_use]
    pub fn csrf(&self) -> String {
        self.between("name=\"csrf\" value=\"", "\"")
    }

    /// The text between the first `start` after which `end` follows.
    ///
    /// # Panics
    ///
    /// If `start` or `end` is not in the body.
    #[must_use]
    pub fn between(&self, start: &str, end: &str) -> String {
        let from = self
            .body
            .find(start)
            .expect("the start marker is in the page")
            + start.len();
        let len = self.body[from..].find(end).expect("terminator");
        self.body[from..from + len].to_owned()
    }
}

/// A browser: the router and one cookie jar.
pub struct Browser {
    router: Router,
    /// The session cookie value the jar holds, if any.
    pub cookie: Option<String>,
}

impl Browser {
    /// A browser with an empty cookie jar, talking to `server`.
    #[must_use]
    pub fn new(server: &TestServer) -> Self {
        Self {
            router: router(server.app_state()),
            cookie: None,
        }
    }

    /// Sends `request` and reads the whole response. A `coffer_session` cookie the response sets (or clears) replaces the jar's.
    ///
    /// # Panics
    ///
    /// If the router fails or the body cannot be read.
    pub async fn send(&mut self, request: Request<Body>) -> Reply {
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
            bytes: body.to_vec(),
        }
    }

    /// A request builder for `method path` carrying the jar's cookie.
    #[must_use]
    pub fn builder(&self, method: Method, path: &str) -> axum::http::request::Builder {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(cookie) = &self.cookie {
            builder = builder.header(header::COOKIE, format!("coffer_session={cookie}"));
        }
        builder
    }

    /// `GET path`.
    ///
    /// # Panics
    ///
    /// If the request cannot be built or sent.
    pub async fn get(&mut self, path: &str) -> Reply {
        let request = self
            .builder(Method::GET, path)
            .body(Body::empty())
            .expect("request");
        self.send(request).await
    }

    /// Posts a form from the public origin.
    ///
    /// The `Origin` header is set by hand. A real browser sends it on a form
    /// POST only because pages carry `Referrer-Policy: same-origin`; under
    /// `no-referrer` it would send `Origin: null` and the origin rule would
    /// refuse the form.
    ///
    /// # Panics
    ///
    /// If the request cannot be built or sent.
    pub async fn post_form(&mut self, path: &str, fields: &[(&str, &str)]) -> Reply {
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

    /// Posts a form claiming to come from `origin`.
    ///
    /// # Panics
    ///
    /// If the request cannot be built or sent.
    pub async fn post_form_from(
        &mut self,
        path: &str,
        origin: &str,
        fields: &[(&str, &str)],
    ) -> Reply {
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

    /// Posts JSON with an optional `Origin` and anti-forgery header.
    ///
    /// # Panics
    ///
    /// If the request cannot be built or sent.
    pub async fn post_json_with(
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

    /// Posts JSON from the public origin.
    ///
    /// # Panics
    ///
    /// If the request cannot be built or sent.
    pub async fn post_json(&mut self, path: &str, body: &Value) -> Reply {
        self.post_json_with(path, Some(TEST_PUBLIC_URL), None, body)
            .await
    }

    /// Redeems an invite or setup link with `auth`: begin, register, finish.
    ///
    /// # Panics
    ///
    /// If the begin step fails or its options cannot be read.
    pub async fn enroll(
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
        assert_eq!(begun.status, StatusCode::OK, "`begun` status");
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

    /// Signs in as `username` with `auth`: begin, sign, finish.
    ///
    /// # Panics
    ///
    /// If the begin step fails or its options cannot be read.
    pub async fn login(&mut self, auth: &mut SoftAuthenticator, username: &str) -> Reply {
        let begun = self
            .post_json("/auth/login/begin", &json!({ "username": username }))
            .await;
        assert_eq!(begun.status, StatusCode::OK, "`begun` status");
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
///
/// # Panics
///
/// If the server cannot be set up or the enrollment fails.
pub async fn with_admin() -> (TestServer, Browser, SoftAuthenticator) {
    let server = TestServer::new().await;
    let link = server
        .identity
        .bootstrap_link()
        .await
        .expect("bootstrap link")
        .expect("a fresh server needs a first admin");
    let mut browser = Browser::new(&server);
    let mut auth = server.authenticator();
    let done = browser.enroll(&mut auth, &link, "alice", "Alice").await;
    assert_eq!(done.status, StatusCode::OK, "`done` status");
    (server, browser, auth)
}
