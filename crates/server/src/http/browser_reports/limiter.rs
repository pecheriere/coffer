//! How many reports reach the log. Kept in memory, so a restart resets it.
//!
//! `PlaidBudget`, the other limiter here, counts per signed-in member and
//! answers how long to wait. This one has anonymous callers, a global bound
//! and nothing to answer, so all it shares with that one is the idea.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

use jiff::{SignedDuration, Timestamp};

/// Reports logged per client per [`WINDOW`]: what `coffer.js` sends at most
/// for one page load.
const PER_CLIENT: u32 = 5;

/// Reports logged per [`WINDOW`] for all clients together. This is the bound
/// on the log's growth, whatever a client claims to be.
const GLOBAL: u32 = 20;

const WINDOW: SignedDuration = SignedDuration::from_mins(1);

/// At least this long passes between two counts of what was dropped.
const SUMMARY_PERIOD: SignedDuration = SignedDuration::from_mins(5);

/// What was dropped since the last summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Dropped {
    pub over_limit: u64,
    pub refused: u64,
}

#[derive(Debug, Default)]
pub(super) struct Limiter {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    window_start: Option<Timestamp>,
    /// Holds at most [`GLOBAL`] entries: one is only added when a report is
    /// admitted, and the map empties with each window.
    per_client: HashMap<Option<IpAddr>, u32>,
    admitted: u32,
    over_limit: u64,
    refused: u64,
    first_drop: Option<Timestamp>,
}

impl Limiter {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // Nothing below can panic and leave the state half changed, so a lock
        // poisoned elsewhere still holds a usable count.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a report from `client` may go on, at `now`. One that may not is
    /// counted.
    pub(super) fn admit(&self, client: Option<IpAddr>, now: Timestamp) -> bool {
        let mut state = self.state();
        let state = &mut *state;
        // A time before the window's start means the clock was set back.
        let current = state.window_start.is_some_and(|start| {
            (SignedDuration::ZERO..WINDOW).contains(&now.duration_since(start))
        });
        if !current {
            state.window_start = Some(now);
            state.per_client.clear();
            state.admitted = 0;
        }
        if state.admitted < GLOBAL {
            let count = state.per_client.entry(client).or_default();
            if *count < PER_CLIENT {
                *count += 1;
                state.admitted += 1;
                return true;
            }
        }
        state.over_limit = state.over_limit.saturating_add(1);
        state.first_drop.get_or_insert(now);
        false
    }

    /// Counts a report refused for its shape or origin.
    pub(super) fn note_refused(&self, now: Timestamp) {
        let mut state = self.state();
        state.refused = state.refused.saturating_add(1);
        state.first_drop.get_or_insert(now);
    }

    /// What was dropped, once [`SUMMARY_PERIOD`] has passed since the first
    /// drop it covers; each drop is reported once. It needs a later request to
    /// be seen, as nothing else wakes this code.
    pub(super) fn due_summary(&self, now: Timestamp) -> Option<Dropped> {
        let mut state = self.state();
        let waited = now.duration_since(state.first_drop?);
        if waited < SignedDuration::ZERO {
            state.first_drop = Some(now);
            return None;
        }
        if waited < SUMMARY_PERIOD {
            return None;
        }
        state.first_drop = None;
        Some(Dropped {
            over_limit: std::mem::take(&mut state.over_limit),
            refused: std::mem::take(&mut state.refused),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use jiff::{SignedDuration, Timestamp};

    use super::{Dropped, GLOBAL, Limiter, PER_CLIENT, SUMMARY_PERIOD, WINDOW};

    fn start() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    fn after(by: SignedDuration) -> Timestamp {
        start().checked_add(by).unwrap()
    }

    fn client(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, last))
    }

    #[test]
    fn the_limits_stay_small() {
        assert_eq!(PER_CLIENT, 5);
        assert_eq!(GLOBAL, 20);
        assert_eq!(WINDOW, SignedDuration::from_mins(1));
        assert_eq!(SUMMARY_PERIOD, SignedDuration::from_mins(5));
    }

    /// Otherwise one client past its limit would use up everyone's reports.
    #[test]
    fn a_client_past_its_limit_spends_nothing_of_the_global_bound() {
        let limiter = Limiter::default();
        for _ in 0..PER_CLIENT + 100 {
            limiter.admit(Some(client(1)), start());
        }
        assert_eq!(limiter.state().admitted, PER_CLIENT);
    }

    #[test]
    fn the_table_of_clients_stays_within_the_global_bound() {
        let limiter = Limiter::default();
        for last in 0..=u8::MAX {
            limiter.admit(Some(client(last)), start());
        }
        assert_eq!(limiter.state().per_client.len(), 20);
    }

    #[test]
    fn a_new_window_forgets_the_clients_of_the_last() {
        let limiter = Limiter::default();
        limiter.admit(Some(client(1)), start());
        limiter.admit(Some(client(2)), after(WINDOW));
        assert_eq!(limiter.state().per_client.len(), 1);
    }

    /// A clock set back must not stop reports until it catches up.
    #[test]
    fn a_clock_set_back_starts_a_new_window() {
        let limiter = Limiter::default();
        for _ in 0..PER_CLIENT {
            assert!(limiter.admit(Some(client(1)), start()));
        }
        assert!(limiter.admit(Some(client(1)), after(SignedDuration::from_hours(-1))));
    }

    #[test]
    fn nothing_dropped_means_no_summary() {
        let limiter = Limiter::default();
        limiter.admit(Some(client(1)), start());
        assert_eq!(limiter.due_summary(after(SUMMARY_PERIOD)), None);
    }

    #[test]
    fn a_summary_waits_for_its_period_and_covers_each_drop_once() {
        let limiter = Limiter::default();
        limiter.note_refused(start());
        let almost = SUMMARY_PERIOD - SignedDuration::from_secs(1);
        assert_eq!(limiter.due_summary(after(almost)), None);
        assert_eq!(
            limiter.due_summary(after(SUMMARY_PERIOD)),
            Some(Dropped {
                over_limit: 0,
                refused: 1
            })
        );
        assert_eq!(limiter.due_summary(after(SUMMARY_PERIOD)), None);
    }

    #[test]
    fn a_clock_set_back_cannot_hasten_or_lose_a_summary() {
        let limiter = Limiter::default();
        limiter.note_refused(start());
        let earlier = after(SignedDuration::from_hours(-1));
        assert_eq!(limiter.due_summary(earlier), None);
        let later = earlier.checked_add(SUMMARY_PERIOD).unwrap();
        assert!(limiter.due_summary(later).is_some());
    }
}
