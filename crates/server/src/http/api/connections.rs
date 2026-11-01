//! `/api/v1/connections`: listing connections, connecting and reconnecting
//! `SimpleFin`, requesting a sync, listing a connection's sync runs and
//! removing a connection.
//!
//! A setup token is a credential: it is read into a `SecretString`, never
//! logged, and no error repeats it. No response ever holds a connection's
//! credentials.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::ConnectionId;
use coffer_domain::sync::SyncTrigger;
use coffer_store::PageRequest;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody, ProblemCode, RateLimitedProblem};
use crate::sync::SyncError;
use crate::sync::connections::ConnectionView;
use crate::sync::engine::SyncRunView;
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

/// Every connection.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectionList {
    pub items: Vec<ConnectionView>,
}

/// Which page of sync runs to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SyncRunQuery {
    /// The `next` cursor of the previous page.
    pub after: Option<String>,
    /// The most runs to return, clamped to 1 to 200.
    #[param(minimum = 0)]
    pub limit: Option<u32>,
}

/// One page of a connection's sync runs, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct SyncRunPage {
    pub items: Vec<SyncRunView>,
    /// Pass as `after` for the next page; `null` on the last page.
    #[schema(required)]
    pub next: Option<String>,
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
                   `422 setup_token_invalid`. Needs `write`; a token with account grants also \
                   needs `all` at `write`, since its first sync queues work on every account it \
                   reports. With an \
                   `Idempotency-Key`, a retry of the same request within 24 hours returns the \
                   first response instead of claiming again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        token (or session) for 24 hours."),
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
    let authorize = async || Ok(state.sync.authorize_connect(&actor).await?);
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
                   connection needs `admin`; a token with account grants also needs `all` at \
                   `write`.",
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

/// Every connection.
#[utoipa::path(
    get,
    path = "/api/v1/connections",
    operation_id = "listConnections",
    tag = tags::CONNECTIONS,
    description = "Every connection, removed ones included, oldest first: its status, last \
                   error (sanitized), last attempt and success, when the schedule syncs it \
                   next, and the provider accounts the token may see. Never its credentials. \
                   Needs `read`. The last error could name any account the connection serves: \
                   a token with account grants gets it only with `all` at `transactions`, \
                   otherwise it is `null` and the status still shows the problem.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The connections.", body = ConnectionList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<ConnectionList>, Problem> {
    let items = state.sync.list_connections(&actor).await?;
    Ok(Json(ConnectionList { items }))
}

/// Requests a sync of a connection.
#[utoipa::path(
    post,
    path = "/api/v1/connections/{connection_id}/sync",
    operation_id = "syncConnection",
    tag = tags::CONNECTIONS,
    description = "Queues a manual sync of the connection, attributed to the token; the server \
                   performs it shortly after. Follow it with \
                   `GET /api/v1/connections/{connection_id}/sync-runs`. The provider's limits \
                   apply: a minimum interval between syncs and a daily budget. Needs `write`; a \
                   token with account grants also needs `all` at `write`.",
    security(("bearer" = [])),
    params(ConnectionPath),
    responses(
        (status = 202, description = "The run, queued.", body = SyncRunView),
        (status = 409, description = "`sync_in_progress` while a run of the connection is \
                                      queued or running; `conflict` when the connection is \
                                      removed.",
         body = ProblemBody, content_type = "application/problem+json"),
        RateLimitedProblem,
        CommonProblems,
    ),
)]
pub(super) async fn sync(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ConnectionPath>,
) -> Result<(StatusCode, Json<SyncRunView>), Problem> {
    let run = state
        .sync
        .request_sync(Some(&actor), path.connection_id, SyncTrigger::Manual)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// A connection's sync runs.
#[utoipa::path(
    get,
    path = "/api/v1/connections/{connection_id}/sync-runs",
    operation_id = "listSyncRuns",
    tag = tags::CONNECTIONS,
    description = "The connection's sync runs, newest first: who requested each, its status \
                   and what it did. Needs `read`; a token with account grants also needs `all` at \
                   `transactions`. Pages with the opaque `next` cursor.",
    security(("bearer" = [])),
    params(ConnectionPath, SyncRunQuery),
    responses(
        (status = 200, description = "One page of runs.", body = SyncRunPage),
        CommonProblems,
    ),
)]
pub(super) async fn list_runs(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ConnectionPath>,
    TypedQuery(query): TypedQuery<SyncRunQuery>,
) -> Result<Json<SyncRunPage>, Problem> {
    let mut page = PageRequest {
        after: query.after,
        ..PageRequest::default()
    };
    if let Some(limit) = query.limit {
        page.limit = limit;
    }
    let page = state
        .sync
        .list_runs(&actor, path.connection_id, &page)
        .await?;
    Ok(Json(SyncRunPage {
        items: page.items,
        next: page.next,
    }))
}

/// Removes a connection.
#[utoipa::path(
    delete,
    path = "/api/v1/connections/{connection_id}",
    operation_id = "removeConnection",
    tag = tags::CONNECTIONS,
    description = "Gives up the connection's access at the provider, erases its credentials \
                   and marks it `removed`. Its accounts, transactions and provider accounts \
                   stay; it is never synced again. Needs `write`; removing another member's \
                   connection needs `admin`; a token with account grants also needs `all` at \
                   `write`.",
    security(("bearer" = [])),
    params(ConnectionPath),
    responses(
        (status = 204, description = "Removed."),
        (status = 409, description = "`conflict` when the connection is already removed, or \
                                      was reconnected meanwhile.",
         body = ProblemBody, content_type = "application/problem+json"),
        (status = 502, description = "`provider_error`: the provider could not give up the \
                                      access; nothing changed.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn remove(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<ConnectionPath>,
) -> Result<StatusCode, Problem> {
    state
        .sync
        .remove_connection(&actor, path.connection_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
