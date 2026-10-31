//! Audit entry types written alongside every mutation.

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

/// One audit record. `changes` must never contain secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub actor: AuditActor,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub changes: serde_json::Value,
}
