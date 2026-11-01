//! `application/problem+json` error bodies (RFC 9457).

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use coffer_domain::policy::{Denied, DenyReason};
use serde::Serialize;
use utoipa::{IntoResponses, ToSchema};

use crate::auth::{AuthError, denied_message};

/// A stable, machine-readable error code. The problem's `type` is
/// `urn:coffer:problem:<code>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemCode {
    Unauthenticated,
    Forbidden,
    CsrfInvalid,
    CrossOrigin,
    NotFound,
    MethodNotAllowed,
    Invalid,
    InvalidCursor,
    Conflict,
    /// A merge would discard the duplicate's user state, and the request
    /// did not ask to.
    MergeConflict,
    InviteUnusable,
    CeremonyExpired,
    Busy,
    PasskeyRejected,
    /// The `Idempotency-Key` was used before for a different request.
    IdempotencyKeyReuse,
    /// A request with this `Idempotency-Key` is still being performed.
    IdempotencyInProgress,
    /// The data provider failed or refused the request.
    ProviderError,
    /// The provider refuses the connection's credentials: reconnect it.
    ReauthRequired,
    /// The setup token is malformed, names a host coffer may not contact,
    /// or was refused by the provider.
    SetupTokenInvalid,
    /// Too many syncs of this connection; `Retry-After` says when to retry.
    SyncRateLimited,
    /// A sync of this connection is already queued or running.
    SyncInProgress,
    /// The provider account's currency is not the account's, or is not a
    /// supported ISO 4217 currency.
    CurrencyMismatch,
    Internal,
}

impl ProblemCode {
    /// The code as it appears in a problem body.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::CsrfInvalid => "csrf_invalid",
            Self::CrossOrigin => "cross_origin",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Invalid => "invalid",
            Self::InvalidCursor => "invalid_cursor",
            Self::Conflict => "conflict",
            Self::MergeConflict => "merge_conflict",
            Self::InviteUnusable => "invite_unusable",
            Self::CeremonyExpired => "ceremony_expired",
            Self::Busy => "busy",
            Self::PasskeyRejected => "passkey_rejected",
            Self::IdempotencyKeyReuse => "idempotency_key_reuse",
            Self::IdempotencyInProgress => "idempotency_in_progress",
            Self::ProviderError => "provider_error",
            Self::ReauthRequired => "reauth_required",
            Self::SetupTokenInvalid => "setup_token_invalid",
            Self::SyncRateLimited => "sync_rate_limited",
            Self::SyncInProgress => "sync_in_progress",
            Self::CurrencyMismatch => "currency_mismatch",
            Self::Internal => "internal",
        }
    }
}

/// An error, as every API operation reports it (RFC 9457).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ProblemBody {
    /// `urn:coffer:problem:<code>`.
    pub r#type: String,
    /// A short, human-readable summary of the kind of problem.
    pub title: String,
    /// The HTTP status code.
    pub status: u16,
    pub code: ProblemCode,
    /// A human-readable explanation of this occurrence. Never echoes input
    /// or a credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The errors any authenticated operation may answer, for its OpenAPI
/// `responses`. Never constructed: handlers return [`Problem`].
#[derive(IntoResponses)]
pub enum CommonProblems {
    /// The request is malformed: an invalid parameter, body or cursor.
    #[response(status = 400, content_type = "application/problem+json")]
    BadRequest(ProblemBody),
    /// The bearer token is missing, unknown, expired or of a disabled member.
    #[response(status = 401, content_type = "application/problem+json")]
    Unauthenticated(ProblemBody),
    /// The token's scope or the member's role does not allow the operation.
    #[response(status = 403, content_type = "application/problem+json")]
    Forbidden(ProblemBody),
    /// No such resource, or no such route.
    #[response(status = 404, content_type = "application/problem+json")]
    NotFound(ProblemBody),
    /// The route does not accept this method; `Allow` lists those it does.
    #[response(status = 405, content_type = "application/problem+json")]
    MethodNotAllowed(ProblemBody),
    /// A submitted value is not acceptable.
    #[response(status = 422, content_type = "application/problem+json")]
    Unprocessable(ProblemBody),
    /// The server failed.
    #[response(status = 500, content_type = "application/problem+json")]
    Internal(ProblemBody),
}

/// The answer of an operation the sync rate limits refuse, for its
/// OpenAPI `responses`. Never constructed: handlers return [`Problem`].
#[derive(IntoResponses)]
pub enum RateLimitedProblem {
    /// `sync_rate_limited`: the connection's limits allow no sync yet.
    #[response(
        status = 429,
        content_type = "application/problem+json",
        headers(("Retry-After" = u32, description = "Whole seconds until a sync is allowed."))
    )]
    RateLimited(ProblemBody),
}

/// Sets `Retry-After` to `seconds` on `headers`: the one way every
/// response, JSON or page, says when to retry.
pub(crate) fn set_retry_after(headers: &mut HeaderMap, seconds: u32) {
    headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
}

/// An HTTP error. Neither field ever carries a presented credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub status: StatusCode,
    /// Stable machine-readable code.
    pub code: ProblemCode,
    pub title: String,
    pub detail: Option<String>,
    /// Seconds after which a retry may succeed, sent as `Retry-After`.
    pub retry_after: Option<u32>,
}

impl Problem {
    /// A problem with no detail.
    #[must_use]
    pub fn new(status: StatusCode, code: ProblemCode, title: &str) -> Self {
        Self {
            status,
            code,
            title: title.to_owned(),
            detail: None,
            retry_after: None,
        }
    }

    /// This problem with `detail`, a human-readable explanation that is
    /// safe to show the requester: never input, a credential or internals.
    #[must_use]
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    /// This problem with a `Retry-After` of `seconds`.
    #[must_use]
    pub fn with_retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    /// The credential is missing, malformed or not accepted.
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ProblemCode::Unauthenticated,
            "Authentication required",
        )
    }

    /// The request is malformed; `detail` says which part, never echoing it.
    #[must_use]
    pub fn invalid_request(detail: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ProblemCode::Invalid,
            "Invalid request",
        )
        .with_detail(detail.to_owned())
    }

    /// No such resource or route.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, ProblemCode::NotFound, "Not found")
    }

    /// The body every API client sees.
    #[must_use]
    pub fn body(&self) -> ProblemBody {
        ProblemBody {
            r#type: format!("urn:coffer:problem:{}", self.code.as_str()),
            title: self.title.clone(),
            status: self.status.as_u16(),
            code: self.code,
            detail: self.detail.clone(),
        }
    }
}

/// The single place a policy denial becomes an HTTP error. A hidden
/// account answers exactly like a missing resource, so its existence never
/// leaks; every other denial is 403 with the reason.
impl From<Denied> for Problem {
    fn from(denied: Denied) -> Self {
        match denied.reason {
            DenyReason::AccountHidden => Self::not_found(),
            DenyReason::SessionRequired
            | DenyReason::InsufficientScope { .. }
            | DenyReason::ScopeAboveRole
            | DenyReason::AccountLevel { .. } => {
                Self::new(StatusCode::FORBIDDEN, ProblemCode::Forbidden, "Forbidden")
                    .with_detail(denied_message(denied))
            }
        }
    }
}

impl From<AuthError> for Problem {
    fn from(err: AuthError) -> Self {
        let message = err.to_string();
        match err {
            AuthError::Denied(denied) => Self::from(denied),
            AuthError::MemberDisabled => {
                Self::new(StatusCode::FORBIDDEN, ProblemCode::Forbidden, "Forbidden")
                    .with_detail(message)
            }
            AuthError::CsrfInvalid => Self::new(
                StatusCode::FORBIDDEN,
                ProblemCode::CsrfInvalid,
                "Request not verified",
            )
            .with_detail(message),
            AuthError::Unauthenticated | AuthError::LoginFailed => Self::unauthenticated(),
            AuthError::NotFound => Self::not_found(),
            AuthError::InviteInvalid | AuthError::InviteUsed => Self::new(
                StatusCode::GONE,
                ProblemCode::InviteUnusable,
                "Link unusable",
            )
            .with_detail(message),
            AuthError::UsernameTaken
            | AuthError::LastPasskey
            | AuthError::LastAdmin
            | AuthError::PasskeyAlreadyRegistered => {
                Self::new(StatusCode::CONFLICT, ProblemCode::Conflict, "Conflict")
                    .with_detail(message)
            }
            AuthError::CeremonyExpired => Self::new(
                StatusCode::BAD_REQUEST,
                ProblemCode::CeremonyExpired,
                "Passkey prompt expired",
            )
            .with_detail(message),
            AuthError::Busy => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ProblemCode::Busy,
                "Too many passkey prompts",
            )
            .with_detail(message),
            AuthError::Invalid(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ProblemCode::Invalid,
                "Invalid value",
            )
            .with_detail(message),
            AuthError::PasskeyRejected(ref source) => {
                tracing::warn!(error = ?source, "passkey response rejected");
                Self::new(
                    StatusCode::BAD_REQUEST,
                    ProblemCode::PasskeyRejected,
                    "Passkey rejected",
                )
                .with_detail(message)
            }
            AuthError::Webauthn(_) | AuthError::Store(_) | AuthError::Internal(_) => {
                tracing::error!(error = ?err, "request failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ProblemCode::Internal,
                    "Internal error",
                )
            }
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body())).into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(seconds) = self.retry_after {
            set_retry_after(headers, seconds);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use coffer_domain::policy::{AccountLevel, Action, Denied, DenyReason};

    use super::{Problem, ProblemCode};
    use crate::auth::AuthError;

    async fn response_bytes(problem: Problem) -> (StatusCode, Vec<(String, Vec<u8>)>, Vec<u8>) {
        let response = problem.into_response();
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
            .collect();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    /// A hidden account must be indistinguishable from a missing one.
    #[tokio::test]
    async fn hidden_account_problem_equals_not_found() {
        let not_found = Problem::from(AuthError::NotFound);
        for needed in [
            AccountLevel::Balances,
            AccountLevel::Transactions,
            AccountLevel::Write,
        ] {
            let denied = Denied {
                action: Action::AccessAccount { needed },
                reason: DenyReason::AccountHidden,
            };
            for hidden in [
                Problem::from(denied),
                Problem::from(AuthError::Denied(denied)),
            ] {
                assert_eq!(
                    serde_json::to_vec(&hidden.body()).unwrap(),
                    serde_json::to_vec(&not_found.body()).unwrap()
                );
                assert_eq!(
                    response_bytes(hidden).await,
                    response_bytes(not_found.clone()).await
                );
            }
        }
    }

    #[test]
    fn account_level_problem_is_forbidden_naming_the_level() {
        let problem = Problem::from(Denied {
            action: Action::AccessAccount {
                needed: AccountLevel::Transactions,
            },
            reason: DenyReason::AccountLevel {
                needed: AccountLevel::Transactions,
            },
        });
        assert_eq!(problem.status, StatusCode::FORBIDDEN);
        assert_eq!(problem.code, ProblemCode::Forbidden);
        assert_eq!(
            problem.detail.as_deref(),
            Some("this action needs transactions access to this account")
        );
    }

    #[test]
    fn codes_serialize_as_their_str() {
        for code in [
            ProblemCode::Unauthenticated,
            ProblemCode::Forbidden,
            ProblemCode::CsrfInvalid,
            ProblemCode::CrossOrigin,
            ProblemCode::NotFound,
            ProblemCode::MethodNotAllowed,
            ProblemCode::Invalid,
            ProblemCode::InvalidCursor,
            ProblemCode::Conflict,
            ProblemCode::MergeConflict,
            ProblemCode::InviteUnusable,
            ProblemCode::CeremonyExpired,
            ProblemCode::Busy,
            ProblemCode::PasskeyRejected,
            ProblemCode::IdempotencyKeyReuse,
            ProblemCode::IdempotencyInProgress,
            ProblemCode::ProviderError,
            ProblemCode::ReauthRequired,
            ProblemCode::SetupTokenInvalid,
            ProblemCode::SyncRateLimited,
            ProblemCode::SyncInProgress,
            ProblemCode::CurrencyMismatch,
            ProblemCode::Internal,
        ] {
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::Value::from(code.as_str())
            );
        }
    }
}
