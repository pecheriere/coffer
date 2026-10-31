//! `GET /api/v1/audit`.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use coffer_domain::audit::AuditActor;
use coffer_domain::policy::{Action, authorize};
use coffer_store::{AuditEvent, AuditFilter, PageRequest, StoreError};
use jiff::Timestamp;
use serde_json::{Value, json};
use url::form_urlencoded;

use crate::auth::AuthError;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::problem::Problem;

/// Audit events, newest first, filtered by the query. The `limit` is
/// clamped by the store.
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    RawQuery(query): RawQuery,
) -> Result<Json<Value>, Problem> {
    authorize(&actor, Action::ReadHousehold).map_err(AuthError::from)?;
    let (filter, page) = parse_query(query.as_deref().unwrap_or_default())?;
    let page = state
        .store
        .list_audit(&filter, &page)
        .await
        .map_err(|err| match err {
            StoreError::InvalidCursor => Problem::new(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "Invalid pagination cursor",
            ),
            other => AuthError::from(other).into(),
        })?;
    Ok(Json(json!({
        "items": page.items.iter().map(event_json).collect::<Vec<_>>(),
        "next": page.next,
    })))
}

/// Parses the query. Errors never echo a supplied value or name. Unknown
/// and repeated parameters are rejected.
fn parse_query(query: &str) -> Result<(AuditFilter, PageRequest), Problem> {
    let mut filter = AuditFilter::default();
    let mut page = PageRequest::default();
    let mut seen: Vec<String> = Vec::new();
    for (name, value) in form_urlencoded::parse(query.as_bytes()) {
        let value = value.into_owned();
        if seen.iter().any(|earlier| earlier == &*name) {
            return Err(invalid_query("a parameter is repeated"));
        }
        seen.push(name.to_string());
        match &*name {
            "entity_type" => filter.entity_type = Some(value),
            "entity_id" => filter.entity_id = Some(value),
            "member" => filter.member = Some(value.parse().map_err(|_| invalid("member"))?),
            "agent" => filter.agent = Some(value.parse().map_err(|_| invalid("agent"))?),
            "from" => filter.from = Some(timestamp(&value, "from")?),
            "to" => filter.to = Some(timestamp(&value, "to")?),
            "after" => page.after = Some(value),
            "limit" => page.limit = value.parse().map_err(|_| invalid("limit"))?,
            _ => return Err(invalid_query("an unknown parameter was supplied")),
        }
    }
    Ok((filter, page))
}

fn timestamp(value: &str, name: &'static str) -> Result<Timestamp, Problem> {
    value.parse().map_err(|_| invalid(name))
}

fn invalid(name: &'static str) -> Problem {
    invalid_query(&format!("the `{name}` parameter is not valid"))
}

fn invalid_query(detail: &str) -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid",
        "Invalid query parameter",
    )
    .with_detail(detail.to_owned())
}

fn event_json(event: &AuditEvent) -> Value {
    let actor = match &event.actor {
        AuditActor::Member { member, agent } => json!({
            "kind": "member",
            "member": member,
            "agent": agent,
        }),
        AuditActor::System { reason } => json!({ "kind": "system", "reason": reason }),
    };
    json!({
        "id": event.id,
        "at": event.at.to_string(),
        "actor": actor,
        "action": event.action,
        "entity_type": event.entity_type,
        "entity_id": event.entity_id,
        "changes": event.changes,
    })
}
