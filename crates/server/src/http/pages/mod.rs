//! Server-rendered pages (maud) and the small JSON endpoints their script
//! uses for passkey ceremonies.
//!
//! Every page route accepts only the session cookie; a bearer token never
//! authenticates one. State-changing requests from a signed-in page must
//! carry the session's anti-forgery token (`csrf` form field or
//! `X-CSRF-Token` header), and every non-safe request must come from the
//! public origin. Setup and invite links hold their token in the path, so
//! nothing here logs request paths, and those pages are never cached or
//! sent as a referrer.

mod account;
mod admin;
mod layout;
mod login;
mod setup;

use axum::extract::rejection::{FormRejection, JsonRejection};
use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, MatchedPath, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use coffer_domain::identity::Actor;
use coffer_domain::ids::MemberId;
use coffer_store::Member;
use serde::de::DeserializeOwned;

use super::AppState;
use super::extract::session_cookie_value;
use super::problem::Problem;
use crate::auth::{AuthError, session_cookie};
use crate::themes::Look;

/// Content Security Policy of every response.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; \
                   form-action 'self'; base-uri 'none'";

/// Largest accepted request body; passkey responses are a few KiB.
const BODY_LIMIT: usize = 64 * 1024;

/// Pages and ceremony endpoints, to be merged into the application router.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(setup::routes())
        .merge(login::routes())
        .merge(account::routes())
        .merge(admin::routes())
        .route("/", get(home))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// `/` sends a signed-in visitor to their account page. Anyone else never
/// gets here: the [`PageSession`] rejection sends them to sign in.
async fn home(_session: PageSession) -> Redirect {
    Redirect::to("/account")
}

pub(super) async fn security_headers(mut response: Response) -> Response {
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    // `same-origin`, not `no-referrer`: under `no-referrer` browsers send
    // `Origin: null` on form posts, which `require_same_origin` refuses.
    // Other sites still get no referrer, so link tokens never leave coffer.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if html {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// Refuses every state-changing request whose `Origin` is missing or not the
/// public origin. Browsers always send `Origin` on cross-site and `fetch`
/// POSTs, so a forged form or request from another site cannot pass.
pub(super) async fn require_same_origin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let safe = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let expected = state.config.public_url.origin().ascii_serialization();
    let from_public_origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == expected);
    // An unmatched path reaches the fallback (404): nothing to protect.
    let routed = request.extensions().get::<MatchedPath>().is_some();
    let bearer_api = request.uri().path().starts_with("/api/");
    if safe || from_public_origin || !routed || bearer_api {
        return next.run(request).await;
    }
    let problem = Problem::new(
        StatusCode::FORBIDDEN,
        "cross_origin",
        "Cross-origin request refused",
    );
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if json {
        problem.into_response()
    } else {
        PageError(problem).into_response()
    }
}

/// A failed page request. Renders a generic HTML error page, or sends an
/// unauthenticated visitor to sign in. It never carries a credential.
#[derive(Debug)]
pub struct PageError(pub Problem);

impl PageError {
    fn invalid(detail: &str) -> Self {
        Self(
            Problem::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "Invalid value")
                .with_detail(detail.to_owned()),
        )
    }

    fn not_found() -> Self {
        Self(Problem::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Not found",
        ))
    }
}

impl From<AuthError> for PageError {
    fn from(err: AuthError) -> Self {
        Self(Problem::from(err))
    }
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let Problem {
            status,
            title,
            detail,
            ..
        } = self.0;
        if status == StatusCode::UNAUTHORIZED {
            return Redirect::to("/login").into_response();
        }
        (status, layout::error_page(&title, detail.as_deref())).into_response()
    }
}

/// A signed-in visitor: who they are, their member record (for the
/// navigation bar and their look) and the cookie that proves it, kept to
/// check the anti-forgery token.
pub struct PageSession {
    actor: Actor,
    member: Member,
    cookie: String,
}

impl PageSession {
    /// The actor behind the cookie.
    fn actor(&self) -> Actor {
        self.actor
    }

    fn cookie(&self) -> &str {
        &self.cookie
    }

    /// The signed-in member.
    fn member(&self) -> &Member {
        &self.member
    }

    /// How this visitor's pages are drawn.
    pub(super) async fn look(&self, state: &AppState) -> Look {
        state.appearance.look(Some(&self.member)).await
    }

    /// The anti-forgery token to embed in this visitor's forms.
    fn csrf(&self, state: &AppState) -> String {
        state.sessions.csrf_for(&self.cookie)
    }

    /// Checks `presented` against the session's anti-forgery token.
    fn verify(&self, state: &AppState, presented: &str) -> Result<Actor, AuthError> {
        state.sessions.check_csrf(&self.cookie, presented)?;
        Ok(self.actor)
    }
}

impl FromRequestParts<AppState> for PageSession {
    type Rejection = PageError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookie = session_cookie_value(&parts.headers)
            .ok_or_else(|| PageError::from(AuthError::Unauthenticated))?
            .to_owned();
        let actor = state.sessions.resolve(&cookie).await?;
        let member = state
            .store
            .get_member(actor.member)
            .await
            .map_err(AuthError::from)?;
        Ok(Self {
            actor,
            member,
            cookie,
        })
    }
}

/// Starts a session for `member` after a successful ceremony: sets the
/// cookie and tells the script where to go.
/// A session already held by the browser is ended first, so signing in
/// again never leaves the old one alive.
async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    member: MemberId,
) -> Result<Response, AuthError> {
    if let Some(old) = session_cookie_value(headers) {
        state.sessions.logout(old).await?;
    }
    let token = state.sessions.create(member).await?;
    let cookie = session_cookie(&token, state.config.session_absolute);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({ "redirect": "/account" })),
    )
        .into_response())
}

/// Status for a body that could not be read: too large stays 413, anything
/// else is 422. The message never echoes the parser's text.
fn unreadable(status: StatusCode) -> StatusCode {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        status
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    }
}

/// A form body whose rejection is the generic error page.
pub struct PageForm<T>(pub T);

impl<T: DeserializeOwned> FromRequest<AppState> for PageForm<T> {
    type Rejection = PageError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        Form::<T>::from_request(request, state)
            .await
            .map(|Form(value)| Self(value))
            .map_err(|rejection: FormRejection| {
                PageError(
                    Problem::new(unreadable(rejection.status()), "invalid", "Invalid request")
                        .with_detail("the form could not be read".to_owned()),
                )
            })
    }
}

/// A JSON body whose rejection is a generic problem.
pub struct ApiJson<T>(pub T);

impl<T: DeserializeOwned> FromRequest<AppState> for ApiJson<T> {
    type Rejection = Problem;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection: JsonRejection| {
                Problem::new(unreadable(rejection.status()), "invalid", "Invalid request")
                    .with_detail("the request body could not be read".to_owned())
            })
    }
}
