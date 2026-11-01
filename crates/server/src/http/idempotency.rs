//! Idempotent creates. A client may send `Idempotency-Key` with a create:
//! the first request with a key is performed, a retry with the same key and
//! request gets the first response back, and a different request with the
//! same key is refused.
//!
//! [`reserve`] records the key as pending in its own transaction; the
//! service that creates the resource completes it (with the response) in
//! the transaction that writes the resource and its audit entry, so the
//! three commit together. A create that fails releases the key.

use axum::Json;
use axum::extract::OptionalFromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse as _, Response};
use coffer_domain::identity::Actor;
use coffer_domain::ids::MemberId;
use coffer_domain::policy::{Action, authorize};
use coffer_store::Reservation;
use serde::Serialize;
use zeroize::Zeroizing;

use super::AppState;
use super::problem::{Problem, ProblemCode};
use crate::auth::AuthError;
use crate::secrets::ServerKey;

/// The request header carrying the key.
pub const HEADER: &str = "Idempotency-Key";

const KEY_INVALID: &str = "the `Idempotency-Key` header is not valid";
const MAX_KEY_LEN: usize = 128;

/// A valid `Idempotency-Key`: 1 to 128 visible ASCII characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Accepts 1 to 128 visible ASCII characters (`!` to `~`).
    #[must_use]
    pub fn parse(value: &[u8]) -> Option<Self> {
        let valid =
            (1..=MAX_KEY_LEN).contains(&value.len()) && value.iter().all(u8::is_ascii_graphic);
        valid
            .then(|| std::str::from_utf8(value).ok().map(str::to_owned))
            .flatten()
            .map(Self)
    }
}

impl<S: Send + Sync> OptionalFromRequestParts<S> for IdempotencyKey {
    type Rejection = Problem;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send {
        std::future::ready(header_key(parts))
    }
}

/// The request's key, `None` without the header; a malformed or repeated
/// header is refused.
fn header_key(parts: &Parts) -> Result<Option<IdempotencyKey>, Problem> {
    let mut values = parts.headers.get_all(HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(Problem::invalid_request(KEY_INVALID));
    }
    IdempotencyKey::parse(value.as_bytes())
        .map(Some)
        .ok_or_else(|| Problem::invalid_request(KEY_INVALID))
}

/// A key reserved for the request being performed. Only [`reserve`] makes
/// one; the creating service completes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencySlot {
    pub(crate) member: MemberId,
    pub(crate) key: String,
}

/// What [`reserve`] decided.
#[derive(Debug)]
pub enum Reserved {
    /// No key was sent: perform the request.
    None,
    /// The key is reserved: perform the request and complete the slot.
    Fresh(IdempotencySlot),
    /// The same request completed already: send this response.
    Replay(Response),
}

/// Hashes requests under a key derived from the server key, so a stored
/// hash reveals nothing of a request body (which may hold a credential).
pub struct RequestHasher(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for RequestHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RequestHasher(<redacted>)")
    }
}

impl RequestHasher {
    /// The hasher keyed for idempotency requests.
    #[must_use]
    pub fn new(key: &ServerKey) -> Self {
        Self(key.derived("coffer 2026-10 idempotency request"))
    }

    /// Keyed BLAKE3 of `u32 len ‖ method ‖ u32 len ‖ path ‖ body`, lengths
    /// big-endian, so no two requests share an encoding. `None` if the
    /// method or path is longer than a `u32` can count.
    #[must_use]
    pub fn hash(&self, method: &Method, path: &str, body: &[u8]) -> Option<[u8; 32]> {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        for part in [method.as_str().as_bytes(), path.as_bytes()] {
            let len = u32::try_from(part.len()).ok()?;
            hasher.update(&len.to_be_bytes());
            hasher.update(part);
        }
        hasher.update(body);
        Some(*hasher.finalize().as_bytes())
    }
}

/// Reserves `key` for `actor`'s request `method path body`.
///
/// # Errors
///
/// `422 idempotency_key_reuse` if the key was used for a different
/// request; `409 idempotency_in_progress` while the same request is still
/// being performed; `500` on storage failures.
pub async fn reserve(
    state: &AppState,
    actor: &Actor,
    key: Option<IdempotencyKey>,
    method: &Method,
    path: &str,
    body: &[u8],
) -> Result<Reserved, Problem> {
    let Some(IdempotencyKey(key)) = key else {
        return Ok(Reserved::None);
    };
    let hash = state
        .idempotency
        .hash(method, path, body)
        .ok_or_else(|| Problem::invalid_request("the request is too large"))?;
    let reservation = async {
        let mut tx = state.store.begin().await?;
        let reservation = tx
            .reserve_idempotency(actor.member, &key, hash, state.clock.now())
            .await?;
        tx.commit().await?;
        Ok::<_, coffer_store::StoreError>(reservation)
    }
    .await
    .map_err(|err| Problem::from(AuthError::from(err)))?;
    match reservation {
        Reservation::Fresh => Ok(Reserved::Fresh(IdempotencySlot {
            member: actor.member,
            key,
        })),
        Reservation::Replay {
            status,
            response_json,
        } => {
            let status = StatusCode::from_u16(status)
                .map_err(|_| Problem::from(AuthError::Internal("stored status is invalid")))?;
            let mut response = (status, response_json).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            Ok(Reserved::Replay(response))
        }
        Reservation::Mismatch => Err(Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ProblemCode::IdempotencyKeyReuse,
            "Idempotency key reused",
        )
        .with_detail("this Idempotency-Key was used for a different request".to_owned())),
        Reservation::InProgress => Err(Problem::new(
            StatusCode::CONFLICT,
            ProblemCode::IdempotencyInProgress,
            "Request in progress",
        )
        .with_detail(
            "a request with this Idempotency-Key is still in progress; retry later".to_owned(),
        )),
    }
}

/// Releases `slot` after its create failed, so a retry is performed rather
/// than refused as in progress. A failure to release is logged: the key
/// then frees itself once the reservation counts as abandoned.
pub async fn release(state: &AppState, slot: &IdempotencySlot) {
    let released = async {
        let mut tx = state.store.begin().await?;
        tx.release_idempotency(slot.member, &slot.key).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = released {
        tracing::warn!(error = ?error, "idempotency key not released");
    }
}

/// An idempotent create as received: its optional key and what the
/// request hash covers.
#[derive(Debug, Clone)]
pub struct CreateRequest<'a> {
    pub key: Option<IdempotencyKey>,
    pub method: &'a Method,
    pub uri: &'a Uri,
    pub body: &'a [u8],
}

/// Performs a create made with an optional `Idempotency-Key`, answering
/// `201 Created` with its result: runs `authorize` before reserving the
/// key (so a key is never reserved by, nor replays a response to, a token
/// that could not have made the request), replays a completed request, and
/// releases the key when the create fails.
///
/// # Errors
///
/// The problem `authorize` returns; the errors of [`reserve`]; the
/// create's own error as a problem.
pub async fn create<T, E, A, F>(
    state: &AppState,
    actor: &Actor,
    request: CreateRequest<'_>,
    authorize: A,
    create: F,
) -> Result<Response, Problem>
where
    T: Serialize,
    E: Into<Problem>,
    A: AsyncFnOnce() -> Result<(), Problem>,
    F: AsyncFnOnce(Option<IdempotencySlot>) -> Result<T, E>,
{
    authorize().await?;
    let slot = match reserve(
        state,
        actor,
        request.key,
        request.method,
        request.uri.path(),
        request.body,
    )
    .await?
    {
        Reserved::None => None,
        Reserved::Fresh(slot) => Some(slot),
        Reserved::Replay(response) => return Ok(response),
    };
    match create(slot.clone()).await {
        Ok(view) => Ok((StatusCode::CREATED, Json(view)).into_response()),
        Err(err) => {
            if let Some(slot) = &slot {
                release(state, slot).await;
            }
            Err(err.into())
        }
    }
}

/// The pre-reserve check of a create of household data: `WriteHousehold`.
///
/// # Errors
///
/// `403` without `WriteHousehold`.
pub fn write_household(actor: &Actor) -> Result<(), Problem> {
    Ok(authorize(actor, Action::WriteHousehold)?)
}
