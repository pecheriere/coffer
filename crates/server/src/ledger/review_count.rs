//! How many transactions wait for someone to review them: the number Home and the review
//! queue show, read without listing them.

use coffer_domain::identity::Actor;
use coffer_domain::policy::AccountLevel;
use coffer_domain::transactions::ReviewStatus;
use coffer_store::TransactionFilter;
use serde::Serialize;
use utoipa::ToSchema;

use super::{Ledger, LedgerError};

/// How many transactions wait for someone to review them, in the accounts the actor reads at
/// `transactions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReviewCount {
    /// Unreviewed transactions in the accounts the token may see. A token that sees only some
    /// accounts counts only theirs.
    pub unreviewed: u64,
}

impl Ledger {
    /// Counts the unreviewed transactions the actor may list: the review queue's length.
    ///
    /// # Errors
    ///
    /// A store error.
    pub async fn count_transactions_to_review(
        &self,
        actor: &Actor,
    ) -> Result<ReviewCount, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let filter = TransactionFilter {
            scope: access.scope(AccountLevel::Transactions),
            accounts: Vec::new(),
            from: None,
            to: None,
            review: Some(ReviewStatus::Unreviewed),
            assignee: None,
            categories: Vec::new(),
            uncategorized: false,
            unmatched_transfer: false,
            rule: None,
            pending: None,
            flag: None,
            min_minor: None,
            max_minor: None,
            text: None,
        };
        let unreviewed = self.store.count_transactions(&filter).await?;
        Ok(ReviewCount { unreviewed })
    }
}
