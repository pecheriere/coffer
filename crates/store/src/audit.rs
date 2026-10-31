//! Append-only audit log.

use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry, AuditValue};
use coffer_domain::ids::{AgentTokenId, AuditEventId, MemberId};
use jiff::Timestamp;
use serde_json::{Map, Value};
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
    pub changes: AuditChanges,
}

/// `changes_version` of rows written by this code.
const CHANGES_VERSION: i64 = 2;

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
              action, entity_type, entity_id, changes_json, changes_version) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(CHANGES_VERSION)
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
             action, entity_type, entity_id, changes_json, changes_version \
             FROM audit_events WHERE 1 = 1",
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
    let action = get("action")?;
    let json = get("changes_json")?;
    let changes = match row
        .try_get::<i64, _>("changes_version")
        .map_err(StoreError::from_sqlx)?
    {
        1 => legacy_changes(&action, &json)?,
        CHANGES_VERSION => serde_json::from_str(&json).map_err(StoreError::backend)?,
        _ => return Err(corrupt("unknown audit changes version")),
    };
    Ok(AuditEvent {
        id: get("id")?.parse().map_err(StoreError::backend)?,
        at,
        actor,
        action,
        entity_type: get("entity_type")?,
        entity_id: get("entity_id")?,
        changes,
    })
}

/// Reads a version-1 `changes_json`, written before changes were typed.
/// `member.role` and `household.theme` stored a bare `{from, to}` for their
/// one field; elsewhere a field holding exactly `{from, to}` was a change
/// and any other value was set. Values without a scalar form are kept as
/// their compact JSON text.
fn legacy_changes(action: &str, json: &str) -> Result<AuditChanges, StoreError> {
    let Value::Object(object) = serde_json::from_str(json).map_err(StoreError::backend)? else {
        return Err(corrupt("audit changes are not an object"));
    };
    let field = match action {
        "member.role" => Some("role"),
        "household.theme" => Some("theme"),
        _ => None,
    };
    if let Some(field) = field
        && let Some((from, to)) = from_to(&object)
    {
        return Ok(AuditChanges::new().change(field, legacy_value(from), legacy_value(to)));
    }
    Ok(object
        .iter()
        .fold(AuditChanges::new(), |changes, (field, value)| {
            match value.as_object().and_then(from_to) {
                Some((from, to)) => changes.change(field, legacy_value(from), legacy_value(to)),
                None => changes.set(field, legacy_value(value)),
            }
        }))
}

/// The two values of an object with exactly the keys `from` and `to`.
fn from_to(object: &Map<String, Value>) -> Option<(&Value, &Value)> {
    match (object.len(), object.get("from"), object.get("to")) {
        (2, Some(from), Some(to)) => Some((from, to)),
        _ => None,
    }
}

fn legacy_value(value: &Value) -> AuditValue {
    match value {
        Value::Null => AuditValue::Null,
        Value::Bool(flag) => AuditValue::Bool(*flag),
        Value::String(text) => AuditValue::Text(text.clone()),
        Value::Number(number) => number
            .as_i64()
            .map_or_else(|| AuditValue::Text(number.to_string()), AuditValue::Int),
        Value::Array(_) | Value::Object(_) => AuditValue::Text(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::audit::{AuditChanges, AuditValue};
    use coffer_domain::ids::MemberId;

    use super::*;

    async fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn changes_roundtrip_typed() {
        let (_dir, store) = open().await;
        let member = MemberId::new();
        let changes = AuditChanges::new()
            .set("member", member)
            .set("enabled", true)
            .set("count", 42_i64)
            .set("expires_at", None::<Timestamp>)
            .change("role", "member", "admin")
            .unset("label", "kitchen");
        let mut tx = store.begin().await.unwrap();
        tx.append_audit(
            Timestamp::from_millisecond(1_000).unwrap(),
            &AuditEntry {
                actor: AuditActor::System {
                    reason: "test".into(),
                },
                action: "from.to".into(),
                entity_type: "member".into(),
                entity_id: member.to_string(),
                changes: changes.clone(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let version: i64 = sqlx::query_scalar("SELECT changes_version FROM audit_events")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(version, 2);
        let events = store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap();
        assert_eq!(events.items[0].changes, changes);
    }

    #[tokio::test]
    async fn legacy_rows_are_read_as_typed_changes() {
        let (_dir, store) = open().await;
        let rows = [
            ("passkey.remove", r#"{"label":{"from":"k","to":null}}"#),
            ("member.role", r#"{"from":"member","to":"admin"}"#),
            ("household.theme", r#"{"from":null,"to":"mochi"}"#),
            ("other.action", r#"{"from":"a","to":"b"}"#),
            ("member.create", r#"{"username":"bob"}"#),
            (
                "member.mixed",
                r#"{"n":3,"ok":false,"list":[1, 2],"obj":{"from":1}}"#,
            ),
        ];
        for (action, changes) in rows {
            // Inserted as 0.1.0 and the design system wrote them: no version.
            sqlx::query(
                "INSERT INTO audit_events \
                 (id, at, actor_kind, actor_system_reason, action, entity_type, entity_id, \
                  changes_json) \
                 VALUES (?, 0, 'system', 'system:cli', ?, 'x', 'x', ?)",
            )
            .bind(AuditEventId::new().to_string())
            .bind(action)
            .bind(changes)
            .execute(&store.pool)
            .await
            .unwrap();
        }

        let events = store
            .list_audit(&AuditFilter::default(), &PageRequest::default())
            .await
            .unwrap();
        let read = |action: &str| {
            events
                .items
                .iter()
                .find(|event| event.action == action)
                .unwrap()
                .changes
                .clone()
        };
        assert_eq!(
            read("passkey.remove"),
            AuditChanges::new().unset("label", "k")
        );
        assert_eq!(
            read("member.role"),
            AuditChanges::new().change("role", "member", "admin")
        );
        assert_eq!(
            read("household.theme"),
            AuditChanges::new().change("theme", AuditValue::Null, "mochi")
        );
        assert_eq!(
            read("other.action"),
            AuditChanges::new().set("from", "a").set("to", "b")
        );
        assert_eq!(
            read("member.create"),
            AuditChanges::new().set("username", "bob")
        );
        assert_eq!(
            read("member.mixed"),
            AuditChanges::new()
                .set("n", 3_i64)
                .set("ok", false)
                .set("list", "[1,2]")
                .set("obj", r#"{"from":1}"#)
        );
    }

    #[tokio::test]
    async fn audit_is_append_only() {
        let (_dir, store) = open().await;
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
                changes: AuditChanges::new(),
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
