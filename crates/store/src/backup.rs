//! Consistent online backup.

use std::io;
use std::path::Path;

use crate::{Store, StoreError};

impl Store {
    /// Write a consistent copy of the database to `dest` using `VACUUM INTO`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Conflict`] if `dest` already exists; otherwise a backend
    /// error if the copy fails.
    pub async fn backup_to(&self, dest: &Path) -> Result<(), StoreError> {
        if dest.try_exists().map_err(StoreError::backend)? {
            return Err(StoreError::Conflict("backup destination already exists"));
        }
        let dest = dest.to_str().ok_or_else(|| {
            StoreError::backend(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backup path is not valid UTF-8",
            ))
        })?;
        sqlx::query("VACUUM INTO ?")
            .bind(dest)
            .execute(&self.pool)
            .await
            .map_err(StoreError::from_sqlx)?;
        Ok(())
    }
}
