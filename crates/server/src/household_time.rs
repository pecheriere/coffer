//! What day it is for the household (spec §8.7).
//!
//! An admin may choose the household's zone in its settings, and until then, or
//! once they clear it, `COFFER_TIMEZONE` applies. It is read from the database every
//! time it is asked for, never kept: a change has to reach the next request and the
//! next job without a restart. Everything that asks what today is, or which day an
//! instant falls on, asks here.

use std::sync::Arc;

use coffer_domain::time::{Clock, IanaZone};
use coffer_store::{Store, StoreError, StoreTx};
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// The household's zone and day, as they are at the moment of asking.
#[derive(Clone)]
pub struct HouseholdTime {
    store: Store,
    clock: Arc<dyn Clock>,
    /// The zone that applies while none is chosen, from `COFFER_TIMEZONE`.
    default_zone: TimeZone,
}

impl std::fmt::Debug for HouseholdTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HouseholdTime").finish_non_exhaustive()
    }
}

impl HouseholdTime {
    /// The household's time over `store`, with `default_zone` while none is chosen.
    #[must_use]
    pub fn new(store: Store, clock: Arc<dyn Clock>, default_zone: TimeZone) -> Self {
        Self {
            store,
            clock,
            default_zone,
        }
    }

    /// The zone that applies while none is chosen.
    #[must_use]
    pub fn default_zone(&self) -> &TimeZone {
        &self.default_zone
    }

    /// The zone that applies given what is `chosen`.
    #[must_use]
    pub fn applying(&self, chosen: Option<&IanaZone>) -> TimeZone {
        chosen.map_or_else(|| self.default_zone.clone(), |zone| zone.zone().clone())
    }

    /// The name of the zone that applies given what is `chosen`.
    #[must_use]
    pub fn applying_name(&self, chosen: Option<&IanaZone>) -> String {
        chosen.map_or_else(
            || self.default_name().to_owned(),
            |zone| zone.name().to_owned(),
        )
    }

    /// The name of the zone that applies while none is chosen. `COFFER_TIMEZONE` is
    /// only ever a named zone, and unset it means `UTC`.
    #[must_use]
    pub fn default_name(&self) -> &str {
        self.default_zone.iana_name().unwrap_or("UTC")
    }

    /// The household's day right now, in `zone`.
    #[must_use]
    pub fn today_in(&self, zone: &TimeZone) -> Date {
        self.clock.now().to_zoned(zone.clone()).date()
    }

    /// The zone that applies now.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored name that is not a zone.
    pub async fn zone(&self) -> Result<TimeZone, StoreError> {
        Ok(self.applying(self.store.household_zone().await?.as_ref()))
    }

    /// The household's day now.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored name that is not a zone.
    pub async fn today(&self) -> Result<Date, StoreError> {
        Ok(self.today_in(&self.zone().await?))
    }

    /// The household's day as `tx` sees it. A writer whose change depends on the day
    /// asks this, under the write lock, so a change of zone can't land between its
    /// asking and its writing.
    ///
    /// # Errors
    ///
    /// Backend failures, including a stored name that is not a zone.
    pub async fn today_seen_by(&self, tx: &mut StoreTx) -> Result<Date, StoreError> {
        let chosen = tx.household_zone().await?;
        Ok(self.today_in(&self.applying(chosen.as_ref())))
    }
}
