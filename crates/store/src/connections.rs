//! Connections to data providers.

use std::str::FromStr;

use coffer_domain::accounts::ConnectionLabel;
use coffer_domain::ids::{ConnectionId, MemberId};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteExecutor};

use crate::convert::{corrupt, parsed, text, text_opt, time, time_opt};
use crate::members::expect_one;
use crate::{Store, StoreError, StoreTx};

macro_rules! columns {
    () => {
        "id, provider, label, owner_id, credentials, provider_state, status, last_error, \
         last_attempt_at, last_success_at, created_at, removed_at"
    };
}

/// The longest provider name.
const MAX_PROVIDER_NAME: usize = 32;

/// Where a connection stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionStatus {
    /// Syncing normally.
    Active,
    /// The last sync failed.
    Error,
    /// The provider refuses the credentials; a member must reconnect.
    ReauthRequired,
    /// Removed by a member; its credentials are gone.
    Removed,
}

impl ConnectionStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Error => "error",
            Self::ReauthRequired => "reauth_required",
            Self::Removed => "removed",
        }
    }
}

/// A connection status the store does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown connection status")]
pub struct UnknownConnectionStatus;

impl FromStr for ConnectionStatus {
    type Err = UnknownConnectionStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "error" => Ok(Self::Error),
            "reauth_required" => Ok(Self::ReauthRequired),
            "removed" => Ok(Self::Removed),
            _ => Err(UnknownConnectionStatus),
        }
    }
}

/// A stored connection. Its `Debug` output never shows the credentials,
/// even sealed.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionRecord {
    pub id: ConnectionId,
    /// The provider's name, such as `simplefin`.
    pub provider: String,
    pub label: ConnectionLabel,
    /// The member who set it up.
    pub owner: MemberId,
    /// Sealed credentials; `None` once removed.
    pub credentials: Option<Vec<u8>>,
    /// Opaque provider state, such as a sync cursor.
    pub provider_state: Option<String>,
    pub status: ConnectionStatus,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<Timestamp>,
    pub last_success_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub removed_at: Option<Timestamp>,
}

impl std::fmt::Debug for ConnectionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionRecord")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("label", &self.label)
            .field("owner", &self.owner)
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "[sealed]"),
            )
            .field("status", &self.status)
            .field("last_attempt_at", &self.last_attempt_at)
            .field("last_success_at", &self.last_success_at)
            .field("created_at", &self.created_at)
            .field("removed_at", &self.removed_at)
            .finish_non_exhaustive()
    }
}

/// A connection to create, `active`.
#[derive(Clone)]
pub struct NewConnection<'a> {
    /// The provider's name: 1 to 32 lowercase ASCII letters, digits or
    /// underscores.
    pub provider: &'a str,
    pub label: ConnectionLabel,
    pub owner: MemberId,
    /// The sealed credentials.
    pub credentials: Vec<u8>,
    pub created_at: Timestamp,
}

impl std::fmt::Debug for NewConnection<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewConnection")
            .field("provider", &self.provider)
            .field("label", &self.label)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The outcome of an attempt to use a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionStatusChange {
    pub status: ConnectionStatus,
    /// Replaces the last error; `None` clears it.
    pub last_error: Option<String>,
    /// When set, the new `last_attempt_at`.
    pub attempted_at: Option<Timestamp>,
    /// When set, the new `last_success_at`.
    pub succeeded_at: Option<Timestamp>,
    /// When the change was made: the removal time for
    /// [`ConnectionStatus::Removed`].
    pub at: Timestamp,
}

/// Whether `name` is a well-formed provider name.
fn valid_provider(name: &str) -> bool {
    (1..=MAX_PROVIDER_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn connection_from_row(row: &SqliteRow) -> Result<ConnectionRecord, StoreError> {
    let provider = text(row, "provider")?;
    if !valid_provider(&provider) {
        return Err(corrupt("malformed provider name"));
    }
    Ok(ConnectionRecord {
        id: parsed(row, "id")?,
        provider,
        label: ConnectionLabel::parse(&text(row, "label")?).map_err(StoreError::backend)?,
        owner: parsed(row, "owner_id")?,
        credentials: row.try_get("credentials").map_err(StoreError::from_sqlx)?,
        provider_state: text_opt(row, "provider_state")?,
        status: parsed(row, "status")?,
        last_error: text_opt(row, "last_error")?,
        last_attempt_at: time_opt(row, "last_attempt_at")?,
        last_success_at: time_opt(row, "last_success_at")?,
        created_at: time(row, "created_at")?,
        removed_at: time_opt(row, "removed_at")?,
    })
}

async fn fetch_connection<'e>(
    executor: impl SqliteExecutor<'e>,
    id: ConnectionId,
) -> Result<ConnectionRecord, StoreError> {
    let row = sqlx::query(concat!(
        "SELECT ",
        columns!(),
        " FROM connections WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(executor)
    .await
    .map_err(StoreError::from_sqlx)?;
    connection_from_row(&row)
}

impl StoreTx {
    /// Create an `active` connection.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for a malformed provider name; backend
    /// failures, including an owner that does not exist.
    pub async fn insert_connection(
        &mut self,
        new: NewConnection<'_>,
    ) -> Result<ConnectionRecord, StoreError> {
        if !valid_provider(new.provider) {
            return Err(StoreError::Invalid("malformed provider name"));
        }
        let id = ConnectionId::new();
        sqlx::query(
            "INSERT INTO connections (id, provider, label, owner_id, credentials, status, \
             created_at) VALUES (?, ?, ?, ?, ?, 'active', ?)",
        )
        .bind(id.to_string())
        .bind(new.provider)
        .bind(new.label.as_str())
        .bind(new.owner.to_string())
        .bind(new.credentials.as_slice())
        .bind(new.created_at.as_millisecond())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        fetch_connection(&mut *self.tx, id).await
    }

    /// Record the outcome of an attempt to use a connection.
    ///
    /// # Errors
    ///
    /// `NotFound` if the connection does not exist.
    pub async fn update_connection_status(
        &mut self,
        id: ConnectionId,
        change: ConnectionStatusChange,
    ) -> Result<(), StoreError> {
        let removed_at =
            (change.status == ConnectionStatus::Removed).then_some(change.at.as_millisecond());
        let result = sqlx::query(
            "UPDATE connections SET status = ?, last_error = ?, \
             last_attempt_at = COALESCE(?, last_attempt_at), \
             last_success_at = COALESCE(?, last_success_at), removed_at = ? WHERE id = ?",
        )
        .bind(change.status.as_str())
        .bind(change.last_error)
        .bind(change.attempted_at.map(Timestamp::as_millisecond))
        .bind(change.succeeded_at.map(Timestamp::as_millisecond))
        .bind(removed_at)
        .bind(id.to_string())
        .execute(&mut *self.tx)
        .await
        .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Replace a connection's sealed credentials; `None` erases them.
    ///
    /// # Errors
    ///
    /// `NotFound` if the connection does not exist.
    pub async fn set_connection_credentials(
        &mut self,
        id: ConnectionId,
        credentials: Option<Vec<u8>>,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE connections SET credentials = ? WHERE id = ?")
            .bind(credentials)
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }

    /// Replace a connection's opaque provider state.
    ///
    /// # Errors
    ///
    /// `NotFound` if the connection does not exist.
    pub async fn set_provider_state(
        &mut self,
        id: ConnectionId,
        state: Option<String>,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE connections SET provider_state = ? WHERE id = ?")
            .bind(state)
            .bind(id.to_string())
            .execute(&mut *self.tx)
            .await
            .map_err(StoreError::from_sqlx)?;
        expect_one(result.rows_affected())
    }
}

impl Store {
    /// Fetch a connection.
    ///
    /// # Errors
    ///
    /// `NotFound` if there is no such connection.
    pub async fn get_connection(&self, id: ConnectionId) -> Result<ConnectionRecord, StoreError> {
        fetch_connection(&self.pool, id).await
    }

    /// Every connection, removed ones included, oldest first.
    ///
    /// # Errors
    ///
    /// Backend failures, or a row that no longer parses.
    pub async fn list_connections(&self) -> Result<Vec<ConnectionRecord>, StoreError> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM connections ORDER BY id"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::from_sqlx)?;
        rows.iter().map(connection_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionStatus, valid_provider};

    #[test]
    fn provider_names_and_statuses() {
        assert!(valid_provider("simplefin"));
        assert!(valid_provider("csv_v2"));
        for bad in ["", "SimpleFin", "simple-fin", "a b", &"x".repeat(33)] {
            assert!(!valid_provider(bad), "{bad:?}");
        }
        for status in [
            ConnectionStatus::Active,
            ConnectionStatus::Error,
            ConnectionStatus::ReauthRequired,
            ConnectionStatus::Removed,
        ] {
            assert_eq!(status.as_str().parse::<ConnectionStatus>(), Ok(status));
        }
        assert!("Active".parse::<ConnectionStatus>().is_err());
    }
}
