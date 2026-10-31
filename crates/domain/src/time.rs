//! The clock abstraction. All code obtains the current time through [`Clock`].

use jiff::Timestamp;

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
