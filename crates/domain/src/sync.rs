//! What starts a provider sync run and where it stands.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A sync value the domain does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown sync trigger or status")]
pub struct UnknownSyncValue;

/// What queued a sync run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SyncTrigger {
    /// The periodic schedule.
    Schedule,
    /// A member or an agent asked for it.
    Manual,
    /// The connection was just created.
    Initial,
}

impl SyncTrigger {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
            Self::Initial => "initial",
        }
    }
}

impl FromStr for SyncTrigger {
    type Err = UnknownSyncValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "schedule" => Ok(Self::Schedule),
            "manual" => Ok(Self::Manual),
            "initial" => Ok(Self::Initial),
            _ => Err(UnknownSyncValue),
        }
    }
}

/// Where a sync run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SyncRunStatus {
    /// Waiting for the worker.
    Queued,
    /// Being performed.
    Running,
    /// Every provider account was applied.
    Succeeded,
    /// Data was applied, but the provider reported errors, an account
    /// failed or records were skipped.
    Partial,
    /// Nothing was applied: the provider call failed.
    Failed,
}

impl SyncRunStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    /// Whether the run is over.
    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Succeeded | Self::Partial | Self::Failed)
    }
}

impl FromStr for SyncRunStatus {
    type Err = UnknownSyncValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "partial" => Ok(Self::Partial),
            "failed" => Ok(Self::Failed),
            _ => Err(UnknownSyncValue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SyncRunStatus, SyncTrigger};

    #[test]
    fn names_round_trip_and_match_serde() {
        for trigger in [
            SyncTrigger::Schedule,
            SyncTrigger::Manual,
            SyncTrigger::Initial,
        ] {
            assert_eq!(trigger.as_str().parse::<SyncTrigger>(), Ok(trigger));
            assert_eq!(
                serde_json::to_value(trigger).unwrap(),
                serde_json::Value::from(trigger.as_str())
            );
        }
        for status in [
            SyncRunStatus::Queued,
            SyncRunStatus::Running,
            SyncRunStatus::Succeeded,
            SyncRunStatus::Partial,
            SyncRunStatus::Failed,
        ] {
            assert_eq!(status.as_str().parse::<SyncRunStatus>(), Ok(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::from(status.as_str())
            );
        }
        assert!("Manual".parse::<SyncTrigger>().is_err());
        assert!("done".parse::<SyncRunStatus>().is_err());
        assert!(!SyncRunStatus::Running.is_finished());
        assert!(SyncRunStatus::Partial.is_finished());
    }
}
