//! Audit entry types written alongside every mutation.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::identity::{Actor, Via};
use crate::ids::{AgentTokenId, MemberId};

/// Who performed an audited change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditActor {
    Member {
        member: MemberId,
        agent: Option<AgentTokenId>,
    },
    System {
        reason: String,
    },
}

impl From<&Actor> for AuditActor {
    fn from(actor: &Actor) -> Self {
        let agent = match actor.via {
            Via::Session => None,
            Via::Agent { token, .. } => Some(token),
        };
        Self::Member {
            member: actor.member,
            agent,
        }
    }
}

/// One audited value: a JSON scalar. Ids, instants (RFC 3339) and calendar
/// days (ISO 8601) are recorded as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuditValue {
    Null,
    Bool(bool),
    Int(i64),
    Text(String),
}

impl From<bool> for AuditValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for AuditValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<&str> for AuditValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for AuditValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Timestamp> for AuditValue {
    fn from(value: Timestamp) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Date> for AuditValue {
    fn from(value: Date) -> Self {
        Self::Text(value.to_string())
    }
}

impl<T: Into<AuditValue>> From<Option<T>> for AuditValue {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

/// One field's value before and after a change; [`AuditValue::Null`] on
/// the side where the field did not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldChange {
    pub before: AuditValue,
    pub after: AuditValue,
}

/// The fields an audited change touched, by name. Serialized as a JSON
/// object `{field: {"before": value, "after": value}}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuditChanges(BTreeMap<String, FieldChange>);

impl AuditChanges {
    /// No changed fields.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `field` as newly set to `value`.
    #[must_use]
    pub fn set(self, field: &str, value: impl Into<AuditValue>) -> Self {
        self.change(field, AuditValue::Null, value)
    }

    /// Records `field` going from `before` to `after`.
    #[must_use]
    pub fn change(
        mut self,
        field: &str,
        before: impl Into<AuditValue>,
        after: impl Into<AuditValue>,
    ) -> Self {
        self.0.insert(
            field.to_owned(),
            FieldChange {
                before: before.into(),
                after: after.into(),
            },
        );
        self
    }

    /// Records `field`, previously `before`, as removed.
    #[must_use]
    pub fn unset(self, field: &str, before: impl Into<AuditValue>) -> Self {
        self.change(field, before, AuditValue::Null)
    }

    /// The changed fields in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &FieldChange)> {
        self.0
            .iter()
            .map(|(field, change)| (field.as_str(), change))
    }

    /// Whether no field changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One audit record. `changes` must never contain secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub actor: AuditActor,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub changes: AuditChanges,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuditChanges, AuditValue};

    #[test]
    fn changes_serialize_as_before_after_objects() {
        let changes = AuditChanges::new()
            .set("name", "x")
            .change("role", "member", "admin");
        assert_eq!(
            serde_json::to_value(&changes).unwrap(),
            json!({
                "name": {"before": null, "after": "x"},
                "role": {"before": "member", "after": "admin"},
            })
        );
    }

    #[test]
    fn option_none_is_null() {
        assert_eq!(AuditValue::from(None::<&str>), AuditValue::Null);
        assert_eq!(AuditValue::from(Some(7_i64)), AuditValue::Int(7));
    }
}
