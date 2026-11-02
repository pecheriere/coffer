//! How often a member's Link pages may call Plaid. Kept in memory, so a
//! restart resets it. Only signed-in members with write access reach it, so
//! it holds at most a window's worth of times per member of the household.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use coffer_domain::ids::MemberId;
use jiff::{SignedDuration, Timestamp};

use crate::sync::SyncError;

/// Requests per member per [`WINDOW`]: well above a member retrying a bank
/// login a few times, and far below what a script could otherwise send.
const REQUESTS_PER_WINDOW: usize = 20;

/// The sliding window [`REQUESTS_PER_WINDOW`] counts requests in.
const WINDOW: SignedDuration = SignedDuration::from_mins(10);

/// The requests each member made to Plaid within the window, oldest first.
#[derive(Debug, Default)]
pub(in crate::sync) struct PlaidBudget {
    spent: Mutex<HashMap<MemberId, Vec<Timestamp>>>,
}

impl PlaidBudget {
    /// Spends one of `member`'s requests at `now`. Call it after every other
    /// reason to refuse and right before calling Plaid, so that a failed call
    /// counts and a refused request does not.
    ///
    /// # Errors
    ///
    /// [`SyncError::PlaidBudgetSpent`] when the window holds no more requests.
    pub(in crate::sync) fn spend(&self, member: MemberId, now: Timestamp) -> Result<(), SyncError> {
        // Nothing below can panic and leave the map half changed, so a lock
        // poisoned elsewhere still holds a usable budget.
        let mut spent = self.spent.lock().unwrap_or_else(PoisonError::into_inner);
        // A time ahead of `now` means the clock was set back: dropping it
        // keeps every wait within the window. Sweeping every member keeps
        // only live entries, so the map stays bounded by the household.
        let live =
            |at: &Timestamp| (SignedDuration::ZERO..WINDOW).contains(&now.duration_since(*at));
        spent.retain(|_, times| {
            times.retain(live);
            !times.is_empty()
        });
        let times = spent.entry(member).or_default();
        if times.len() >= REQUESTS_PER_WINDOW
            && let Some(oldest) = times.first()
        {
            return Err(SyncError::PlaidBudgetSpent {
                retry_after: WINDOW.saturating_sub(now.duration_since(*oldest)),
            });
        }
        times.push(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::ids::MemberId;
    use jiff::{SignedDuration, Timestamp};

    use super::{PlaidBudget, REQUESTS_PER_WINDOW, WINDOW};
    use crate::sync::SyncError;

    fn spend_all(budget: &PlaidBudget, member: MemberId, now: Timestamp) {
        for _ in 0..REQUESTS_PER_WINDOW {
            assert!(budget.spend(member, now).is_ok(), "within the budget");
        }
    }

    fn wait(budget: &PlaidBudget, member: MemberId, now: Timestamp) -> Option<SignedDuration> {
        match budget.spend(member, now) {
            Err(SyncError::PlaidBudgetSpent { retry_after }) => Some(retry_after),
            _ => None,
        }
    }

    fn start() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    fn after(at: Timestamp, by: SignedDuration) -> Timestamp {
        at.checked_add(by).unwrap()
    }

    #[test]
    fn the_limit_stays_generous_and_small() {
        assert_eq!(REQUESTS_PER_WINDOW, 20);
        assert_eq!(WINDOW, SignedDuration::from_mins(10));
    }

    #[test]
    fn refuses_past_the_limit_until_the_oldest_request_leaves_the_window() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        spend_all(&budget, member, start());

        assert_eq!(wait(&budget, member, start()), Some(WINDOW));
        let almost = after(start(), WINDOW - SignedDuration::from_secs(1));
        assert_eq!(
            wait(&budget, member, almost),
            Some(SignedDuration::from_secs(1))
        );
        assert_eq!(wait(&budget, member, after(start(), WINDOW)), None);
    }

    #[test]
    fn the_window_slides_with_each_request() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        let later = after(start(), SignedDuration::from_mins(5));
        for _ in 0..REQUESTS_PER_WINDOW / 2 {
            budget.spend(member, start()).unwrap();
        }
        for _ in 0..REQUESTS_PER_WINDOW / 2 {
            budget.spend(member, later).unwrap();
        }

        let freed = after(start(), WINDOW);
        for _ in 0..REQUESTS_PER_WINDOW / 2 {
            assert_eq!(wait(&budget, member, freed), None, "the older half left");
        }
        assert_eq!(
            wait(&budget, member, freed),
            Some(SignedDuration::from_mins(5)),
            "the newer half has five minutes left"
        );
    }

    #[test]
    fn a_refused_request_does_not_extend_the_wait() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        spend_all(&budget, member, start());

        let middle = after(start(), SignedDuration::from_mins(9));
        assert!(wait(&budget, member, middle).is_some());
        assert_eq!(wait(&budget, member, after(start(), WINDOW)), None);
    }

    #[test]
    fn members_have_budgets_of_their_own() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        spend_all(&budget, member, start());

        assert!(budget.spend(MemberId::new(), start()).is_ok());
        assert_eq!(wait(&budget, member, start()), Some(WINDOW));
    }

    #[test]
    fn expired_entries_of_every_member_are_dropped() {
        let budget = PlaidBudget::default();
        for _ in 0..8 {
            budget.spend(MemberId::new(), start()).unwrap();
        }
        assert_eq!(budget.spent.lock().unwrap().len(), 8);

        let last = MemberId::new();
        budget.spend(last, after(start(), WINDOW)).unwrap();

        let spent = budget.spent.lock().unwrap();
        assert_eq!(spent.len(), 1, "only the live member stays");
        assert!(spent.contains_key(&last));
    }

    #[test]
    fn a_member_holds_no_more_than_the_limit() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        spend_all(&budget, member, start());
        for _ in 0..REQUESTS_PER_WINDOW {
            assert!(wait(&budget, member, start()).is_some());
        }

        assert_eq!(
            budget.spent.lock().unwrap().get(&member).map(Vec::len),
            Some(REQUESTS_PER_WINDOW)
        );
    }

    /// A clock set back must not lock a member out until it catches up.
    #[test]
    fn a_clock_set_back_never_waits_longer_than_the_window() {
        let budget = PlaidBudget::default();
        let member = MemberId::new();
        spend_all(&budget, member, start());

        let earlier = after(start(), SignedDuration::from_hours(-24));
        assert_eq!(wait(&budget, member, earlier), None);
    }
}
