//! `application/problem+json` error bodies (RFC 9457).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::AuthError;

/// An HTTP error. Neither field ever carries a presented credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub status: StatusCode,
    /// Stable machine-readable code.
    pub code: &'static str,
    pub title: String,
    pub detail: Option<String>,
}

impl Problem {
    /// A problem with no detail.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, title: &str) -> Self {
        Self {
            status,
            code,
            title: title.to_owned(),
            detail: None,
        }
    }

    /// This problem with `detail`, a human-readable explanation that is
    /// safe to show the requester: never input, a credential or internals.
    #[must_use]
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The credential is missing, malformed or not accepted.
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Authentication required",
        )
    }
}

impl From<AuthError> for Problem {
    fn from(err: AuthError) -> Self {
        let message = err.to_string();
        match err {
            AuthError::Denied(_) | AuthError::MemberDisabled => {
                Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden").with_detail(message)
            }
            AuthError::CsrfInvalid => Self::new(
                StatusCode::FORBIDDEN,
                "csrf_invalid",
                "Request not verified",
            )
            .with_detail(message),
            AuthError::Unauthenticated | AuthError::LoginFailed => Self::unauthenticated(),
            AuthError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found", "Not found"),
            AuthError::InviteInvalid | AuthError::InviteUsed => {
                Self::new(StatusCode::GONE, "invite_unusable", "Link unusable").with_detail(message)
            }
            AuthError::UsernameTaken
            | AuthError::LastPasskey
            | AuthError::LastAdmin
            | AuthError::PasskeyAlreadyRegistered => {
                Self::new(StatusCode::CONFLICT, "conflict", "Conflict").with_detail(message)
            }
            AuthError::CeremonyExpired => Self::new(
                StatusCode::BAD_REQUEST,
                "ceremony_expired",
                "Passkey prompt expired",
            )
            .with_detail(message),
            AuthError::Busy => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "Too many passkey prompts",
            )
            .with_detail(message),
            AuthError::Invalid(_) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", "Invalid value")
                    .with_detail(message)
            }
            AuthError::PasskeyRejected(ref source) => {
                tracing::warn!(error = ?source, "passkey response rejected");
                Self::new(
                    StatusCode::BAD_REQUEST,
                    "passkey_rejected",
                    "Passkey rejected",
                )
                .with_detail(message)
            }
            AuthError::Webauthn(_) | AuthError::Store(_) | AuthError::Internal(_) => {
                tracing::error!(error = ?err, "request failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "Internal error",
                )
            }
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut body = json!({
            "type": "about:blank",
            "title": self.title,
            "status": self.status.as_u16(),
            "code": self.code,
        });
        if let (Some(detail), Value::Object(map)) = (self.detail, &mut body) {
            map.insert("detail".to_owned(), Value::String(detail));
        }
        let mut response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
