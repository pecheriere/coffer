//! A Plaid sync cursor held for good, and letting it move on.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{ConnectionId, ProviderAccountId};
use coffer_domain::policy::{AccountLevel, Action, authorize};
use coffer_store::ProviderAccountRecord;
use serde::Serialize;
use utoipa::ToSchema;

use super::audit;
use crate::sync::SyncError;
use crate::sync::engine::{SyncService, ensure_not_removed};

const NOT_HELD: &str = "the connection's sync cursor is not held";

/// How many syncs in a row must leave the cursor in place because of one
/// provider account before the connection says it is held.
pub const HELD_AFTER_SYNCS: u32 = 3;

/// What the connection says about a held cursor. Fixed text: nothing of the
/// provider's is repeated.
pub const CURSOR_HELD_TEXT: &str = "The sync cursor is held: records of the listed provider \
     accounts keep failing to store, so every sync fetches them again.";

/// A sync cursor that stays in place because records of some provider
/// accounts keep failing to store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HeldCursorView {
    /// Fixed text.
    pub text: &'static str,
    /// The provider accounts whose records hold the cursor.
    pub provider_accounts: Vec<ProviderAccountId>,
    /// Whether a member already asked for the cursor to move past those
    /// records: the next sync does.
    pub release_pending: bool,
}

/// Whether a sync cursor is held for good by `accounts`, all of one
/// connection: they are the provider accounts that left it in place for
/// [`HELD_AFTER_SYNCS`] syncs in a row.
pub(super) fn held_cursor(accounts: &[ProviderAccountRecord]) -> Option<HeldCursorView> {
    let mut holding: Vec<&ProviderAccountRecord> = accounts
        .iter()
        .filter(|account| account.held_syncs >= HELD_AFTER_SYNCS)
        .collect();
    if holding.is_empty() {
        return None;
    }
    holding.sort_by_key(|account| account.id.to_string());
    Some(HeldCursorView {
        text: CURSOR_HELD_TEXT,
        release_pending: holding.iter().all(|account| account.hold_released),
        provider_accounts: holding.iter().map(|account| account.id).collect(),
    })
}

impl SyncService {
    /// Lets the connection's sync cursor move past the records that keep
    /// failing to store: the next sync passes those of every provider
    /// account that held it for [`HELD_AFTER_SYNCS`] syncs, for good, and
    /// only counts them as skipped. Returns those provider accounts. The
    /// records are lost to coffer, so this needs a passkey session (never an
    /// agent token) of the connection's owner or an admin, with write
    /// access to every account like removing the connection. The change is
    /// audited, without any provider text.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may; [`SyncError::NotFound`]
    /// for an unknown connection; [`SyncError::Conflict`] for a removed one
    /// or one whose cursor is not held; storage failures.
    pub async fn release_held_cursor(
        &self,
        actor: &Actor,
        id: ConnectionId,
    ) -> Result<Vec<ProviderAccountId>, SyncError> {
        authorize(actor, Action::ManageConnections)?;
        self.authorize_every_account(actor, AccountLevel::Write)
            .await?;
        let current = self.store.get_connection(id).await?;
        authorize(
            actor,
            Action::ReleaseHeldCursor {
                owner: current.owner,
            },
        )?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        ensure_not_removed(&mut tx, id).await?;
        let released = tx.release_cursor_holds(id, HELD_AFTER_SYNCS).await?;
        if released.is_empty() {
            return Err(SyncError::Conflict(NOT_HELD));
        }
        let accounts = released
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        audit(
            &mut tx,
            now,
            actor,
            "connection.release_cursor",
            ("connection", id.to_string()),
            AuditChanges::new().set("provider_accounts", accounts),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(connection = %id, accounts = released.len(), "held sync cursor released");
        Ok(released)
    }
}
