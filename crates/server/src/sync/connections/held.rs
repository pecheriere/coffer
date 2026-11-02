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
const HOLD_CHANGED: &str = "what holds the sync cursor changed; check the page and try again";

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
    /// How many records of those accounts the last sync fetched and could
    /// not store: what a release is about.
    pub records: u32,
    /// Whether a member already asked for the cursor to move past those
    /// records: a sync does, as soon as no other account blocks it and the
    /// database and the provider's answer allow trying them.
    pub release_pending: bool,
    /// Whether such a request was withdrawn because what is wrong with one
    /// of the accounts, or how many records it has, changed after the member
    /// asked. Nothing was passed,
    /// and the cursor moves on only if a member looks again and asks again.
    pub release_withdrawn: bool,
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
        records: holding.iter().map(|account| account.held_records).sum(),
        release_pending: holding.iter().all(|account| account.hold_released),
        release_withdrawn: holding.iter().any(|account| account.release_withdrawn),
        provider_accounts: holding.iter().map(|account| account.id).collect(),
    })
}

impl SyncService {
    /// Lets the connection's sync cursor move past the records that keep
    /// failing to store. `shown` are the provider accounts the member saw
    /// holding it and `shown_records` how many records they held: if that is
    /// no longer exactly what holds it, nothing changes. The count is what
    /// the member agrees to: a fetch with more records for an account takes
    /// its release back. The next sync then applies those accounts' records one by
    /// one and gives up only the ones that still fail, for good; they are
    /// counted as skipped and the pass is audited with its count. That
    /// happens only if the sync moves the cursor, so a failure elsewhere or
    /// a database fault keeps the release for a later one. A refusal of the
    /// account other than the one recorded withdraws it, for a member to look
    /// again. The records are lost to coffer, so
    /// this needs a passkey session (never an agent token) of the
    /// connection's owner or an admin, with write access to every account
    /// like removing the connection. The request is audited with its count
    /// and each account's recorded error, coffer's own fixed text for a Plaid
    /// account, and names no provider record.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] unless the actor may; [`SyncError::NotFound`]
    /// for an unknown connection; [`SyncError::Conflict`] for a removed one,
    /// one whose cursor is not held, or one whose holding accounts or record
    /// count are not what was shown; storage failures.
    pub async fn release_held_cursor(
        &self,
        actor: &Actor,
        id: ConnectionId,
        shown: &[ProviderAccountId],
        shown_records: u32,
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
        let holding = tx.held_provider_accounts(id, HELD_AFTER_SYNCS).await?;
        if holding.is_empty() {
            return Err(SyncError::Conflict(NOT_HELD));
        }
        let mut shown = shown.to_vec();
        shown.sort_by_key(ToString::to_string);
        shown.dedup();
        if shown != holding {
            return Err(SyncError::Conflict(HOLD_CHANGED));
        }
        let mut records = 0_u32;
        let mut refusals = Vec::new();
        for held in &holding {
            let account = tx.get_provider_account(*held).await?;
            records = records.saturating_add(account.held_records);
            if let Some(error) = account.error {
                refusals.push(format!("{held}: {error}"));
            }
        }
        if shown_records != records {
            return Err(SyncError::Conflict(HOLD_CHANGED));
        }
        tx.release_cursor_holds(&holding).await?;
        let accounts = holding
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
            AuditChanges::new()
                .set("provider_accounts", accounts)
                .set("records", i64::from(records))
                .set("refusals", refusals.join("; ")),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(connection = %id, accounts = holding.len(), "held sync cursor released");
        Ok(holding)
    }
}
