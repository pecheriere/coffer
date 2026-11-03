//! The clock abstraction. All code obtains the current time through [`Clock`].

use jiff::Timestamp;
use jiff::tz::{TimeZone, TimeZoneDatabase};

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// The real wall clock. The only place `Timestamp::now()` is permitted.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A name that isn't an IANA time zone. Says nothing of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a known IANA time zone name")]
pub struct UnknownZone;

/// A time zone chosen by its IANA name, spelled as IANA spells it.
///
/// What counts as a name is the copy of the database built into the binary, so
/// it doesn't depend on the machine: a system's `localtime` or `posix/...` is
/// not one, and a name is never used to find a file until that copy has it. A
/// system copy is still preferred for the zone's rules, since it may be newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IanaZone {
    name: String,
    zone: TimeZone,
}

impl IanaZone {
    /// The zone called `input`, in any case.
    ///
    /// # Errors
    ///
    /// [`UnknownZone`] for anything the built-in database doesn't name.
    pub fn parse(input: &str) -> Result<Self, UnknownZone> {
        let bundled = TimeZoneDatabase::bundled()
            .get(input)
            .map_err(|_| UnknownZone)?;
        let name = bundled.iana_name().ok_or(UnknownZone)?.to_owned();
        let zone = jiff::tz::db().get(&name).unwrap_or(bundled);
        Ok(Self { name, zone })
    }

    /// Every name [`Self::parse`] accepts, sorted.
    #[must_use]
    pub fn all_names() -> Vec<String> {
        let mut names: Vec<String> = TimeZoneDatabase::bundled()
            .available()
            .map(|name| name.as_str().to_owned())
            .collect();
        names.sort_unstable();
        names
    }

    /// The name, spelled as IANA spells it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The zone itself.
    #[must_use]
    pub fn zone(&self) -> &TimeZone {
        &self.zone
    }
}

#[cfg(feature = "testkit")]
pub use manual::ManualClock;

#[cfg(feature = "testkit")]
mod manual {
    use std::sync::{Mutex, PoisonError};

    use jiff::{SignedDuration, Timestamp};

    use super::Clock;

    /// A clock that only moves when told to, for deterministic tests.
    #[derive(Debug)]
    pub struct ManualClock(Mutex<Timestamp>);

    impl ManualClock {
        /// Creates a clock frozen at `start`.
        #[must_use]
        pub fn new(start: Timestamp) -> Self {
            Self(Mutex::new(start))
        }

        /// Moves the clock forward (or backward, for a negative duration),
        /// saturating at the bounds of the representable timestamp range.
        pub fn advance(&self, by: SignedDuration) {
            let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            *now = now.saturating_add(by).unwrap_or(*now);
        }

        /// Sets the clock to an absolute instant.
        pub fn set(&self, to: Timestamp) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = to;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Timestamp {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }
}
