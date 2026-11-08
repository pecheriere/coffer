//! Counting the transactions a filter matches, without reading them.

use coffer_domain::policy::AccountScope;
use sqlx::{QueryBuilder, Sqlite};

use super::{TransactionFilter, push_filter};
use crate::convert::corrupt;
use crate::{Store, StoreError};

impl Store {
    /// How many transactions match `filter`. An empty scope matches none.
    ///
    /// # Errors
    ///
    /// A backend error.
    pub async fn count_transactions(&self, filter: &TransactionFilter) -> Result<u64, StoreError> {
        if matches!(&filter.scope, AccountScope::Only(ids) if ids.is_empty()) {
            return Ok(0);
        }
        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT count(*) FROM transactions WHERE 1 = 1");
        push_filter(&mut query, filter);
        let count: i64 = query
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        u64::try_from(count).map_err(|_| corrupt("a negative count of transactions"))
    }
}
