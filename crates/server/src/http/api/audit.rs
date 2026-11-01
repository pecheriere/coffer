//! `GET /api/v1/audit`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use coffer_domain::audit::{AuditActor, AuditChanges};
use coffer_domain::ids::{AgentTokenId, AuditEventId, MemberId};
use coffer_domain::policy::{AccountLevel, Action, authorize, authorize_every_account};
use coffer_store::{AuditEvent, AuditFilter, PageRequest, StoreError};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::TypedQuery;
use super::openapi::tags;
use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::{CommonProblems, Problem, ProblemCode};

/// Narrowing criteria and paging for the audit log. Unset criteria match
/// anything.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    /// Only events about this kind of entity, for example `member`.
    pub entity_type: Option<String>,
    /// Only events about the entity with this id.
    pub entity_id: Option<String>,
    /// Only changes made by this member (through a session or a token).
    pub member: Option<MemberId>,
    /// Only changes made through this agent token.
    pub agent: Option<AgentTokenId>,
    /// Only events at or after this instant (RFC 3339).
    pub from: Option<Timestamp>,
    /// Only events before this instant (RFC 3339).
    pub to: Option<Timestamp>,
    /// The `next` cursor of the previous page.
    pub after: Option<String>,
    /// The most events to return, clamped to 1 to 200.
    #[param(minimum = 0)]
    pub limit: Option<u32>,
}

/// One page of audit events, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditPage {
    pub items: Vec<AuditEventView>,
    /// Pass as `after` for the next page; `null` on the last page.
    #[schema(required)]
    pub next: Option<String>,
}

/// One audited change.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEventView {
    pub id: AuditEventId,
    /// When the change was made.
    pub at: Timestamp,
    pub actor: AuditActorView,
    /// What was done, for example `member.create`.
    pub action: String,
    /// The kind of entity changed, for example `member`.
    pub entity_type: String,
    /// The id of the entity changed.
    pub entity_id: String,
    pub changes: AuditChanges,
}

/// Who made an audited change.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditActorView {
    /// A member, through a session (`agent` is `null`) or an agent token.
    Member {
        member: MemberId,
        #[schema(required)]
        agent: Option<AgentTokenId>,
    },
    /// coffer itself, for example a provider sync.
    System {
        /// Why coffer made the change, for example `sync`.
        reason: String,
    },
}

impl From<&AuditEvent> for AuditEventView {
    fn from(event: &AuditEvent) -> Self {
        let actor = match &event.actor {
            AuditActor::Member { member, agent } => AuditActorView::Member {
                member: *member,
                agent: *agent,
            },
            AuditActor::System { reason } => AuditActorView::System {
                reason: reason.clone(),
            },
        };
        Self {
            id: event.id,
            at: event.at,
            actor,
            action: event.action.clone(),
            entity_type: event.entity_type.clone(),
            entity_id: event.entity_id.clone(),
            changes: event.changes.clone(),
        }
    }
}

/// Audit events, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/audit",
    operation_id = "listAudit",
    tag = tags::AUDIT,
    description = "Audit events, newest first, narrowed by the query. Needs `read`; a \
                   token with account grants also needs `all` at `transactions`. Pages with the opaque `next` cursor.",
    security(("bearer" = [])),
    params(AuditQuery),
    responses(
        (status = 200, description = "One page of events.", body = AuditPage),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<AuditQuery>,
) -> Result<Json<AuditPage>, Problem> {
    authorize(&actor, Action::ReadHousehold)?;
    // Entries name and describe every account, so a token reads them only
    // if it sees every account's transactions.
    let access = state.access.resolve(&actor).await?;
    authorize_every_account(&actor, &access, AccountLevel::Transactions)?;
    let filter = AuditFilter {
        entity_type: query.entity_type,
        entity_id: query.entity_id,
        member: query.member,
        agent: query.agent,
        from: query.from,
        to: query.to,
    };
    let mut page = PageRequest {
        after: query.after,
        ..PageRequest::default()
    };
    if let Some(limit) = query.limit {
        page.limit = limit;
    }
    let page = state
        .store
        .list_audit(&filter, &page)
        .await
        .map_err(|err| match err {
            StoreError::InvalidCursor => Problem::new(
                StatusCode::BAD_REQUEST,
                ProblemCode::InvalidCursor,
                "Invalid pagination cursor",
            ),
            other => AuthError::from(other).into(),
        })?;
    Ok(Json(AuditPage {
        items: page.items.iter().map(AuditEventView::from).collect(),
        next: page.next,
    }))
}
