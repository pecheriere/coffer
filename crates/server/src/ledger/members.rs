//! The household's members as anyone who may read the household sees them: the one read behind
//! `GET /api/v1/members`, its MCP tool and every page that names members, so they all need what
//! the API needs. Managing members (invites, roles, disabling) stays in
//! [`Identity`](crate::auth::Identity).

use coffer_domain::identity::Actor;
use coffer_domain::policy::{Action, authorize};
use coffer_store::Member;

use super::{Ledger, LedgerError};

impl Ledger {
    /// Every household member, disabled ones included, in the store's order. Read every member
    /// through this rather than the store, so the read is authorized in one place.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; storage failures.
    pub async fn list_members(&self, actor: &Actor) -> Result<Vec<Member>, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        Ok(self.store.list_members().await?)
    }
}
