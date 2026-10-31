//! Append-only audit log.

use coffer_domain::audit::{AuditActor, AuditEntry};
use coffer_domain::ids::{AgentTokenId, AuditEventId, MemberId};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::convert::corrupt;
use crate::{Page, PageRequest, Store, StoreError, StoreTx};

/// A stored audit record.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub at: Timestamp,
    pub actor: AuditActor,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub changes: serde_json::Value,
}

/// Narrowing criteria for [`Store::list_audit`]; unset fields match anything.
/// `from` is inclusive, `to` is exclusive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub member: Option<MemberId>,
    pub agent: Option<AgentTokenId>,
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
}

impl StoreTx {
    /// Record `entry` at `at` inside this transaction.
    ///
    /// # Errors
    ///
    /// Fails if the entry cannot be serialized or written.
    pub async fn append_audit(
        &mut self,
        at: Timestamp,
        entry: &AuditEntry,
    ) -> Result<AuditEventId, StoreError> {
        let id = AuditEventId::new();
        let (kind, member, agent, reason) = match &entry.actor {
            AuditActor::Member { member, agent } => (
                "member",
                Some(member.to_string()),
                agent.map(|a| a.to_string()),
                None,
            ),
            AuditActor::System { reason } => ("system", None, None, Some(reason.as_str())),
        };
        let changes = serde_json::to_string(&entry.changes).map_err(StoreError::backend)?;
        sqlx::query(
            "INSERT INTO audit_events \
             (id, at, actor_kind, actor_member_id, actor_agent_id, actor_system_reason, \
              action, entity_type, entity_id, changes_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(at.as_millisecond())
        .bind(kind)
        .bind(member)
        .bind(agent)
        .bind(reason)
        .bind(&entry.action)
        .bind(&entry.entity_type)
        .bind(&entry.entity_id)
        .bind(changes)
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        Ok(id)
    }
}

impl Store {
    /// List audit events matching `filter`, newest first by event id (ids
    /// are `UUIDv7`, so this is creation order). `filter.from` is inclusive
    /// and `filter.to` exclusive, both compared with the event's `at`. The
    /// returned cursor continues after the last item of the page.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidCursor`] if `page.after` is not a cursor issued by
    /// this method; otherwise a backend error.
    pub async fn list_audit(
        &self,
        filter: &AuditFilter,
        page: &PageRequest,
    ) -> Result<Page<AuditEvent>, StoreError> {
        let limit = page.effective_limit();
        let after = page
            .after
            .as_deref()
            .map(|cursor| {
                cursor
                    .parse::<AuditEventId>()
                    .map_err(|_| StoreError::InvalidCursor)
            })
            .transpose()?;

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, at, actor_kind, actor_member_id, actor_agent_id, actor_system_reason, \
             action, entity_type, entity_id, changes_json FROM audit_events WHERE 1 = 1",
        );
        if let Some(entity_type) = &filter.entity_type {
            query.push(" AND entity_type = ").push_bind(entity_type);
        }
        if let Some(entity_id) = &filter.entity_id {
            query.push(" AND entity_id = ").push_bind(entity_id);
        }
        if let Some(member) = filter.member {
            query
                .push(" AND actor_member_id = ")
                .push_bind(member.to_string());
        }
        if let Some(agent) = filter.agent {
            query
                .push(" AND actor_agent_id = ")
                .push_bind(agent.to_string());
        }
        if let Some(from) = filter.from {
            query.push(" AND at >= ").push_bind(from.as_millisecond());
        }
        if let Some(to) = filter.to {
            query.push(" AND at < ").push_bind(to.as_millisecond());
        }
        if let Some(after) = after {
            query.push(" AND id < ").push_bind(after.to_string());
        }
        // One extra row tells whether another page exists.
        query
            .push(" ORDER BY id DESC LIMIT ")
            .push_bind(i64::from(limit) + 1);

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        let mut items = rows
            .iter()
            .map(audit_event_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|event| event.id.to_string())
        } else {
            None
        };
        Ok(Page { items, next })
    }
}

fn audit_event_from_row(row: &SqliteRow) -> Result<AuditEvent, StoreError> {
    let get = |column: &str| {
        row.try_get::<String, _>(column)
            .map_err(StoreError::from_sqlx)
    };
    let get_opt = |column: &str| {
        row.try_get::<Option<String>, _>(column)
            .map_err(StoreError::from_sqlx)
    };

    let actor = match get("actor_kind")?.as_str() {
        "member" => {
            let member = get_opt("actor_member_id")?
                .ok_or_else(|| corrupt("member actor without member id"))?
                .parse()
                .map_err(StoreError::backend)?;
            let agent = get_opt("actor_agent_id")?
                .map(|a| a.parse())
                .transpose()
                .map_err(StoreError::backend)?;
            AuditActor::Member { member, agent }
        }
        "system" => AuditActor::System {
            reason: get_opt("actor_system_reason")?
                .ok_or_else(|| corrupt("system actor without reason"))?,
        },
        _ => return Err(corrupt("unknown actor kind")),
    };
    let at = Timestamp::from_millisecond(row.try_get("at").map_err(StoreError::from_sqlx)?)
        .map_err(StoreError::backend)?;
    Ok(AuditEvent {
        id: get("id")?.parse().map_err(StoreError::backend)?,
        at,
        actor,
        action: get("action")?,
        entity_type: get("entity_type")?,
        entity_id: get("entity_id")?,
        changes: serde_json::from_str(&get("changes_json")?).map_err(StoreError::backend)?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn audit_is_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.append_audit(
            Timestamp::from_millisecond(1_000).unwrap(),
            &AuditEntry {
                actor: AuditActor::System {
                    reason: "test".into(),
                },
                action: "member.create".into(),
                entity_type: "member".into(),
                entity_id: "m1".into(),
                changes: json!({}),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        for statement in [
            "UPDATE audit_events SET action = 'x'",
            "DELETE FROM audit_events",
        ] {
            let err = sqlx::query(statement)
                .execute(&store.pool)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, sqlx::Error::Database(db) if db.message().contains("append-only")),
                "{statement}: {err:?}"
            );
        }

        let events = store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap();
        assert_eq!(events.items.len(), 1);
        assert_eq!(events.items[0].action, "member.create");
    }

    #[tokio::test]
    async fn actor_columns_must_match_the_actor_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        let insert = |kind: &'static str,
                      member: Option<&'static str>,
                      agent: Option<&'static str>,
                      reason: Option<&'static str>| {
            sqlx::query(
                "INSERT INTO audit_events \
                 (id, at, actor_kind, actor_member_id, actor_agent_id, actor_system_reason, \
                  action, entity_type, entity_id, changes_json) \
                 VALUES (?, 0, ?, ?, ?, ?, 'x', 'x', 'x', '{}')",
            )
            .bind(AuditEventId::new().to_string())
            .bind(kind)
            .bind(member)
            .bind(agent)
            .bind(reason)
            .execute(&store.pool)
        };

        let violations = [
            ("member", None, None, None),
            ("member", Some("m"), None, Some("system:cli")),
            ("system", None, None, None),
            ("system", Some("m"), None, Some("system:cli")),
            ("system", None, Some("a"), Some("system:cli")),
        ];
        for (kind, member, agent, reason) in violations {
            let err = insert(kind, member, agent, reason).await.unwrap_err();
            assert!(
                matches!(&err, sqlx::Error::Database(db) if db.is_check_violation()),
                "{kind} {member:?} {agent:?} {reason:?}: {err:?}"
            );
        }
        insert("member", Some("m"), None, None).await.unwrap();
        insert("member", Some("m"), Some("a"), None).await.unwrap();
        insert("system", None, None, Some("system:cli"))
            .await
            .unwrap();
    }
}
