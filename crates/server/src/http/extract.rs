//! Request authentication: bearer tokens for API routes, the session cookie
//! for page routes. The two never mix: an API route accepts only a bearer
//! token and a page route only the cookie.

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;
use coffer_domain::identity::Actor;

use super::AppState;
use super::problem::Problem;
use crate::auth::COOKIE_NAME;

/// The member behind an `Authorization: Bearer coffer_…` header.
#[derive(Debug, Clone, Copy)]
pub struct ApiActor(pub Actor);

/// The member behind the session cookie.
#[derive(Debug, Clone, Copy)]
pub struct PageActor(pub Actor);

impl FromRequestParts<AppState> for ApiActor {
    type Rejection = Problem;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts).ok_or_else(Problem::unauthenticated)?;
        let actor = state
            .agent_tokens
            .resolve(token)
            .await
            .map_err(Problem::from)?;
        Ok(Self(actor))
    }
}

impl FromRequestParts<AppState> for PageActor {
    type Rejection = Problem;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookie = session_cookie_value(&parts.headers).ok_or_else(Problem::unauthenticated)?;
        let actor = state
            .sessions
            .resolve(cookie)
            .await
            .map_err(Problem::from)?;
        Ok(Self(actor))
    }
}

/// The token of a well-formed `Authorization: Bearer <token>` header.
fn bearer_token(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() && !token.contains(' '))
        .then_some(token)
}

/// The value of the session cookie, if any `Cookie` header carries it.
pub(crate) fn session_cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|header| header.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME && !value.is_empty()).then_some(value)
        })
}
