//! Faults the tests install in the store, to see how the server behaves when
//! a write fails.

use coffer_domain::ids::ConnectionId;
use coffer_store::InjectedRunUpdate;

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

    /// Makes every later update that marks a sync run failed fail with an
    /// error a retry cannot cure, until [`Self::clear_sync_run_failures`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_sync_run_failure(&self) {
        self.store
            .inject_sync_run_failure_for_tests()
            .await
            .expect("install the injected failure");
    }

    /// Makes every later update of a sync run of kind `on` fail as a database
    /// fault would, until [`Self::clear_sync_run_failures`].
    ///
    /// # Panics
    ///
    /// If the failure cannot be installed.
    pub async fn inject_sync_run_fault(&self, on: InjectedRunUpdate) {
        self.store
            .inject_sync_run_fault_for_tests(on)
            .await
            .expect("install the injected failure");
    }

    /// Ends what [`Self::inject_sync_run_failure`] and
    /// [`Self::inject_sync_run_fault`] installed.
    ///
    /// # Panics
    ///
    /// If the failures cannot be removed.
    pub async fn clear_sync_run_failures(&self) {
        self.store
            .clear_sync_run_failures_for_tests()
            .await
            .expect("remove the injected failure");
    }

    /// Deletes the row of `connection` but not its runs, so a run that
    /// started finds the connection gone.
    ///
    /// # Panics
    ///
    /// If the row cannot be deleted.
    pub async fn delete_connection_row(&self, connection: ConnectionId) {
        self.store
            .delete_connection_row_for_tests(connection)
            .await
            .expect("delete the connection row");
    }
}
