//! `/api/v1/connections`: connecting and reconnecting `SimpleFin`.
//!
//! A setup token is a credential: it is read into a `SecretString`, never
//! logged, and no error repeats it.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::identity::Actor;
use coffer_domain::ids::ConnectionId;
use coffer_domain::policy::{Action, authorize};
use secrecy::SecretString;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody, ProblemCode};
use crate::sync::SyncError;
use crate::sync::connections::ConnectionView;
use crate::sync::provider::{ConnectInput, ProviderError, ProviderKind};

const LABEL_INVALID: &str = "label must be 1 to 100 characters without control characters";

/// A `SimpleFin` connection to add.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSimpleFinConnection {
    /// 1 to 100 characters, without control characters.
    pub label: String,
    /// The setup token from the `SimpleFin` Bridge: the base64 of a claim
    /// URL on an allowed bridge host. Claimed once; never stored, logged or
    /// echoed.
    #[schema(value_type = String, format = Password)]
    pub setup_token: SecretString,
}

/// A new setup token for an existing connection.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SimpleFinReconnect {
    /// A new setup token from the `SimpleFin` Bridge.
    #[schema(value_type = String, format = Password)]
    pub setup_token: SecretString,
}

/// One connection.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct ConnectionPath {
    /// The connection's id.
    pub connection_id: ConnectionId,
}

/// The problem of a connect or reconnect: a setup token the provider
/// refuses is `422 setup_token_invalid`, with the provider's fixed text.
pub(crate) fn connect_problem(err: SyncError) -> Problem {
    match err {
        SyncError::Provider(ref provider @ ProviderError::Invalid(_)) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ProblemCode::SetupTokenInvalid,
            "Setup token invalid",
        )
        .with_detail(provider.record_text()),
        other => Problem::from(other),
    }
}

/// The pre-reserve check of a connect: `ManageConnections`.
fn manage_connections(actor: &Actor) -> Result<(), Problem> {
    Ok(authorize(actor, Action::ManageConnections)?)
}

/// Connects `SimpleFin`.
#[utoipa::path(
    post,
    path = "/api/v1/connections/simplefin",
    operation_id = "createSimpleFinConnection",
    tag = tags::CONNECTIONS,
    description = "Claims a `SimpleFin` setup token and stores the access it grants, sealed, as \
                   a new connection owned by the token's member; its first sync is queued. The \
                   claim URL must name an allowed bridge host: a setup token that is \
                   malformed, names another host or is refused by the bridge answers \
                   `422 setup_token_invalid`. Needs `write`. With an \
                   `Idempotency-Key`, a retry of the same request within 24 hours returns the \
                   first response instead of claiming again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        member for 24 hours."),
    ),
    request_body = NewSimpleFinConnection,
    responses(
        (status = 201, description = "The connection, created now or by an earlier request \
                                      with the same `Idempotency-Key`.", body = ConnectionView),
        (status = 409, description = "A request with this `Idempotency-Key` is still in \
                                      progress.",
         body = ProblemBody, content_type = "application/problem+json"),
        (status = 502, description = "`provider_error`: the bridge could not be reached or \
                                      failed.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn create_simplefin(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    method: Method,
    uri: Uri,
    key: Option<IdempotencyKey>,
    TypedJsonBytes { value, bytes }: TypedJsonBytes<NewSimpleFinConnection>,
) -> Result<Response, Problem> {
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || manage_connections(&actor);
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        let label = ConnectionLabel::parse(&value.label)
            .map_err(|_| Problem::from(SyncError::Invalid(LABEL_INVALID)))?;
        state
            .sync
            .create_connection(
                &actor,
                ProviderKind::SimpleFin,
                label,
                ConnectInput::SimpleFinSetupToken(value.setup_token),
                slot,
            )
            .await
            .map_err(connect_problem)
    })
    .await
}

/// Reconnects a connection with a new `SimpleFin` setup token.
#[utoipa::path(
    post,
    path = "/api/v1/connections/{connection_id}/simplefin/reconnect",
    operation_id = "reconnectSimpleFin",
    tag = tags::CONNECTIONS,
    description = "Claims a new setup token for a connection, typically one whose status is \
                   `reauth_required`, and replaces its access: the connection is `active` \
                   again, and its provider accounts and bindings stay. A sync is queued when \
                   the limits allow. A refused setup token answers \
                   `422 setup_token_invalid`. Needs `write`; reconnecting another member's \
                   connection needs `admin`.",
    security(("bearer" = [])),
    params(ConnectionPath),
    request_body = SimpleFinReconnect,
    responses(
        (status = 200, description = "The connection, reconnected.", body = ConnectionView),
        (status = 409, description = "`conflict` when the connection is removed, or was \
                                      reconnected or removed meanwhile.",
         body = ProblemBody, content_type = "application/problem+json"),
        (status = 502, description = "`provider_error`: the bridge could not be reached or \
                                      failed.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn reconnect_simplefin(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ConnectionPath>,
    TypedJson(body): TypedJson<SimpleFinReconnect>,
) -> Result<Json<ConnectionView>, Problem> {
    let view = state
        .sync
        .reconnect(
            &actor,
            path.connection_id,
            ConnectInput::SimpleFinSetupToken(body.setup_token),
        )
        .await
        .map_err(connect_problem)?;
    Ok(Json(view))
}
