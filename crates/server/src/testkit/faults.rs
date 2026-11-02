//! Faults the tests install in the store, to see how the server behaves when
//! a write fails.

use super::TestServer;

impl TestServer {
    /// Makes every later write of a provider record with the id
    /// [`coffer_store::INJECTED_FAILURE_RECORD_ID`] fail, as a storage failure would.
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_source_insert_failure(&self) {
        self.store
            .inject_source_insert_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Makes every later insertion of a source record with the provider id
    /// [`coffer_store::INJECTED_FAULT_RECORD_ID`] fail as a database fault would, until
    /// [`Self::clear_source_insert_fault`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_source_insert_fault(&self) {
        self.store
            .inject_source_insert_fault_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Ends [`Self::inject_source_insert_fault`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be removed.
    pub async fn clear_source_insert_fault(&self) {
        self.store
            .clear_source_insert_fault_for_tests()
            .await
            .expect("remove the injected failure");
    }

    /// Makes the record [`coffer_store::INJECTED_BIND_RECORD_ID`] bind its provider account
    /// to the database's one account as it is stored.
    ///
    /// # Panics
    ///
    /// If the trigger cannot be installed.
    pub async fn inject_bind_on_source_insert(&self) {
        self.store
            .inject_bind_on_source_insert_for_tests()
            .await
            .expect("install the injected binding");
    }

    /// Makes every later insertion of a connection labelled
    /// [`coffer_store::INJECTED_FAILURE_LABEL`] fail, as a storage failure would.
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_connection_insert_failure(&self) {
        self.store
            .inject_connection_insert_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Makes every later update that finishes a sync run fail as a store
    /// outage would, until [`Self::clear_sync_run_finish_failure`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_sync_run_finish_failure(&self) {
        self.store
            .inject_sync_run_finish_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Ends [`Self::inject_sync_run_finish_failure`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be removed.
    pub async fn clear_sync_run_finish_failure(&self) {
        self.store
            .clear_sync_run_finish_failure_for_tests()
            .await
            .expect("remove the injected failure");
    }
}
