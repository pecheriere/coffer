//! Matching v1 (spec §8.4): which canonical transaction an incoming
//! provider record belongs to.
//!
//! The description key is the description's ASCII letters, uppercased,
//! every other character being a separator, reduced to the first two
//! tokens of three or more letters. The fingerprint is
//! `fp1:<amount minor units>:<description key>`.
//!
//! A candidate is an imported transaction of the same account, dated within
//! [`DATE_TOLERANCE_DAYS`], with no live source from the incoming record's
//! connection, and with the same amount; or, when all its sources are
//! pending records that vanished from the current fetch, an amount within
//! [`PENDING_SUCCESSOR_TOLERANCE_PERCENT`] and the same non-empty
//! description key (a pending charge posting with a tip). One candidate
//! attaches; several are narrowed to an equal description key and amount;
//! if still ambiguous the record becomes a new transaction flagged as a
//! possible duplicate.

use jiff::civil::Date;

use crate::ids::TransactionId;

/// The version of [`fingerprint`]'s definition, stored with every
/// fingerprint so the definition can evolve.
pub const FINGERPRINT_VERSION: u8 = 1;

/// How many days apart a record and a candidate may be posted.
pub const DATE_TOLERANCE_DAYS: i64 = 4;

/// How far (in percent of the pending amount) a posted amount may differ
/// from a vanished pending one it replaces.
pub const PENDING_SUCCESSOR_TOLERANCE_PERCENT: i64 = 20;

/// The fewest letters a token needs to count in a description key.
const MIN_TOKEN_LETTERS: usize = 3;

/// How many tokens a description key keeps.
const KEY_TOKENS: usize = 2;

/// The description key of `description`: its first two runs of three or
/// more ASCII letters, uppercased and joined by a space; `""` when it has
/// none.
#[must_use]
pub fn description_key(description: &str) -> String {
    description
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|token| token.len() >= MIN_TOKEN_LETTERS)
        .take(KEY_TOKENS)
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The version-1 fingerprint of a record: `fp1:<amount_minor>:<key>`.
#[must_use]
pub fn fingerprint(amount_minor: i64, description: &str) -> String {
    format!(
        "fp{FINGERPRINT_VERSION}:{amount_minor}:{}",
        description_key(description)
    )
}

/// The record being matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub posted_on: Date,
    pub amount_minor: i64,
    pub description_key: String,
}

/// An imported transaction of the record's account, posted near it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub transaction: TransactionId,
    pub posted_on: Date,
    pub amount_minor: i64,
    pub description_key: String,
    /// It has a source from the record's connection that the current fetch
    /// still reports (vanished sources do not count).
    pub live_source_from_same_connection: bool,
    /// It has sources, and every one is a pending record that vanished from
    /// the current fetch.
    pub only_vanished_pending: bool,
}

/// What to do with an incoming record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchDecision {
    /// Attach it to this transaction.
    Attach(TransactionId),
    /// Create a new transaction.
    New,
    /// Create a new transaction, flagged as possibly the same purchase as
    /// these.
    NewPossibleDuplicate(Vec<TransactionId>),
}

/// Whether `a` and `b` are posted at most [`DATE_TOLERANCE_DAYS`] apart.
fn within_date_tolerance(a: Date, b: Date) -> bool {
    a.since(b)
        .is_ok_and(|span| i64::from(span.get_days()).abs() <= DATE_TOLERANCE_DAYS)
}

/// Whether `amount` is within [`PENDING_SUCCESSOR_TOLERANCE_PERCENT`] of
/// `pending_amount`. Computed in `i128`, so it never overflows.
fn within_successor_tolerance(pending_amount: i64, amount: i64) -> bool {
    let difference = (i128::from(amount) - i128::from(pending_amount)).abs();
    difference * 100
        <= i128::from(pending_amount).abs() * i128::from(PENDING_SUCCESSOR_TOLERANCE_PERCENT)
}

/// Whether two description keys are equal and not empty.
fn same_key(a: &str, b: &str) -> bool {
    !a.is_empty() && a == b
}

/// Whether `candidate` may receive `incoming` (spec §8.4).
#[must_use]
pub fn is_eligible(incoming: &Incoming, candidate: &Candidate) -> bool {
    !candidate.live_source_from_same_connection
        && within_date_tolerance(candidate.posted_on, incoming.posted_on)
        && (candidate.amount_minor == incoming.amount_minor
            || (candidate.only_vanished_pending
                && same_key(&candidate.description_key, &incoming.description_key)
                && within_successor_tolerance(candidate.amount_minor, incoming.amount_minor)))
}

/// Decides where `incoming` goes among `candidates`.
#[must_use]
pub fn decide(incoming: &Incoming, candidates: &[Candidate]) -> MatchDecision {
    let eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| is_eligible(incoming, candidate))
        .collect();
    match eligible.as_slice() {
        [] => return MatchDecision::New,
        [only] => return MatchDecision::Attach(only.transaction),
        _ => {}
    }
    let narrowed: Vec<&&Candidate> = eligible
        .iter()
        .filter(|candidate| {
            candidate.amount_minor == incoming.amount_minor
                && same_key(&candidate.description_key, &incoming.description_key)
        })
        .collect();
    match narrowed.as_slice() {
        [only] => MatchDecision::Attach(only.transaction),
        _ => MatchDecision::NewPossibleDuplicate(
            eligible
                .iter()
                .map(|candidate| candidate.transaction)
                .collect(),
        ),
    }
}

/// The transactions among `pending` that a posted record `incoming` may be
/// the posted form of, while their pending records are still reported
/// beside it. `pending` holds transactions whose sources are all pending
/// records the current fetch of the same connection still reports, which
/// [`decide`] never offers; a match is posted within
/// [`DATE_TOLERANCE_DAYS`] with the same amount, or an amount within
/// [`PENDING_SUCCESSOR_TOLERANCE_PERCENT`] and the same non-empty
/// description key. Such a record becomes a new transaction flagged as a
/// possible duplicate of them, never a silent second transaction.
#[must_use]
pub fn pending_counterparts(incoming: &Incoming, pending: &[Candidate]) -> Vec<TransactionId> {
    pending
        .iter()
        .filter(|candidate| {
            within_date_tolerance(candidate.posted_on, incoming.posted_on)
                && (candidate.amount_minor == incoming.amount_minor
                    || (same_key(&candidate.description_key, &incoming.description_key)
                        && within_successor_tolerance(
                            candidate.amount_minor,
                            incoming.amount_minor,
                        )))
        })
        .map(|candidate| candidate.transaction)
        .collect()
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use proptest::prelude::{Just, Strategy, any, prop_oneof};

    use super::{
        Candidate, DATE_TOLERANCE_DAYS, Incoming, MatchDecision, decide, description_key,
        fingerprint, is_eligible, pending_counterparts,
    };
    use crate::ids::TransactionId;

    fn incoming(day: i8, amount_minor: i64, description: &str) -> Incoming {
        Incoming {
            posted_on: date(2026, 10, day),
            amount_minor,
            description_key: description_key(description),
        }
    }

    fn candidate(day: i8, amount_minor: i64, description: &str) -> Candidate {
        Candidate {
            transaction: TransactionId::new(),
            posted_on: date(2026, 10, day),
            amount_minor,
            description_key: description_key(description),
            live_source_from_same_connection: false,
            only_vanished_pending: false,
        }
    }

    #[test]
    fn description_key_examples() {
        assert_eq!(description_key("SQ *BLUE BOTTLE 1234"), "BLUE BOTTLE");
        assert_eq!(description_key("POS 12/03"), "POS");
        assert_eq!(description_key("12/03 #42"), "", "no qualifying token");
        assert_eq!(description_key(""), "");
        assert_eq!(
            description_key("Blue bottle coffee"),
            "BLUE BOTTLE",
            "letters are uppercased; only two tokens are kept"
        );
        assert_eq!(
            description_key("CAFÉ-NOIR"),
            "CAF NOIR",
            "a non-ASCII letter is a separator"
        );
        assert_eq!(description_key("AB CD EFG"), "EFG");
        assert_eq!(
            fingerprint(-450, "SQ *BLUE BOTTLE 1234"),
            "fp1:-450:BLUE BOTTLE"
        );
        assert_eq!(fingerprint(0, "12/03"), "fp1:0:");
    }

    #[test]
    fn exact_amount_within_tolerance_attaches() {
        let record = incoming(10, -1_250, "CORNER SHOP 0042");
        for day in [6, 10, 14] {
            let near = candidate(day, -1_250, "CORNER SHOP");
            assert_eq!(
                decide(&record, std::slice::from_ref(&near)),
                MatchDecision::Attach(near.transaction),
                "posted on the {day}th"
            );
        }
        let other_amount = candidate(10, -1_251, "CORNER SHOP");
        assert_eq!(decide(&record, &[other_amount]), MatchDecision::New);
    }

    #[test]
    fn outside_tolerance_is_new() {
        let record = incoming(10, -1_250, "CORNER SHOP");
        assert_eq!(
            decide(&record, &[candidate(5, -1_250, "CORNER SHOP")]),
            MatchDecision::New
        );
        assert_eq!(
            decide(&record, &[candidate(15, -1_250, "CORNER SHOP")]),
            MatchDecision::New
        );
    }

    #[test]
    fn two_candidates_same_key_flagged() {
        let record = incoming(10, -500, "SQ *BLUE BOTTLE");
        let first = candidate(9, -500, "BLUE BOTTLE 77");
        let second = candidate(11, -500, "BLUE BOTTLE 78");
        assert_eq!(
            decide(&record, &[first.clone(), second.clone()]),
            MatchDecision::NewPossibleDuplicate(vec![first.transaction, second.transaction])
        );

        let other_key = candidate(11, -500, "HARDWARE STORE");
        assert_eq!(
            decide(&record, &[first.clone(), other_key.clone()]),
            MatchDecision::Attach(first.transaction),
            "narrowing to the equal key resolves the ambiguity"
        );
        let unkeyed = incoming(10, -500, "12/03");
        assert!(
            matches!(
                decide(&unkeyed, &[candidate(10, -500, "1"), candidate(10, -500, "2")]),
                MatchDecision::NewPossibleDuplicate(ids) if ids.len() == 2
            ),
            "empty keys never narrow"
        );
    }

    #[test]
    fn live_same_connection_source_never_candidate() {
        let record = incoming(10, -500, "BLUE BOTTLE");
        let mut sibling = candidate(10, -500, "BLUE BOTTLE");
        sibling.live_source_from_same_connection = true;
        assert_eq!(decide(&record, &[sibling]), MatchDecision::New);
    }

    #[test]
    fn vanished_pending_accepts_a_tip_with_the_same_key() {
        let record = incoming(12, -4_600, "BISTRO CENTRAL 9");
        let mut pending = candidate(10, -4_000, "BISTRO CENTRAL");
        pending.only_vanished_pending = true;
        assert_eq!(
            decide(&record, std::slice::from_ref(&pending)),
            MatchDecision::Attach(pending.transaction)
        );
        assert_eq!(
            decide(
                &incoming(12, -4_801, "BISTRO CENTRAL"),
                std::slice::from_ref(&pending)
            ),
            MatchDecision::New,
            "more than 20 % away"
        );
        assert_eq!(
            decide(
                &incoming(12, -4_800, "BISTRO CENTRAL"),
                std::slice::from_ref(&pending)
            ),
            MatchDecision::Attach(pending.transaction),
            "exactly 20 % away"
        );
        assert_eq!(
            decide(
                &incoming(12, -4_600, "OTHER PLACE"),
                std::slice::from_ref(&pending)
            ),
            MatchDecision::New,
            "another key"
        );
        let mut posted = pending.clone();
        posted.only_vanished_pending = false;
        assert_eq!(
            decide(&record, &[posted]),
            MatchDecision::New,
            "not pending"
        );
    }

    #[test]
    fn pending_counterparts_of_a_posted_record() {
        let record = incoming(12, -4_600, "BISTRO CENTRAL");
        let tip = candidate(10, -4_000, "BISTRO CENTRAL");
        let same = candidate(11, -4_600, "SOMETHING ELSE");
        let far = candidate(1, -4_600, "BISTRO CENTRAL");
        let unrelated = candidate(12, -100, "BISTRO CENTRAL");
        assert_eq!(
            pending_counterparts(&record, &[tip.clone(), same.clone(), far, unrelated]),
            vec![tip.transaction, same.transaction]
        );
    }

    fn arbitrary_candidate() -> impl Strategy<Value = Candidate> {
        (
            0_i8..=20,
            prop_oneof![Just(-1_000_i64), Just(-1_100), Just(-1_300), any::<i64>()],
            prop_oneof![Just("BLUE BOTTLE"), Just("CORNER SHOP"), Just("12")],
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(
                |(day, amount_minor, description, live, vanished)| Candidate {
                    transaction: TransactionId::new(),
                    posted_on: date(2026, 10, 1 + day),
                    amount_minor,
                    description_key: description_key(description),
                    live_source_from_same_connection: live,
                    only_vanished_pending: vanished,
                },
            )
    }

    proptest::proptest! {
        #[test]
        fn decide_never_attaches_to_ineligible(
            day in 0_i8..=20,
            amount_minor in prop_oneof![Just(-1_000_i64), Just(-1_100), any::<i64>()],
            description in prop_oneof![Just("BLUE BOTTLE"), Just("CORNER SHOP"), Just("")],
            candidates in proptest::collection::vec(arbitrary_candidate(), 0..8),
        ) {
            let record = Incoming {
                posted_on: date(2026, 10, 1 + day),
                amount_minor,
                description_key: description_key(description),
            };
            match decide(&record, &candidates) {
                MatchDecision::Attach(id) => {
                    let chosen = candidates.iter().find(|c| c.transaction == id);
                    proptest::prop_assert!(chosen.is_some_and(|c| is_eligible(&record, c)));
                    proptest::prop_assert!(chosen.is_some_and(|c| !c.live_source_from_same_connection));
                    let near = chosen.is_some_and(|c| {
                        i64::from((c.posted_on - record.posted_on).get_days()).abs()
                            <= DATE_TOLERANCE_DAYS
                    });
                    proptest::prop_assert!(near);
                }
                MatchDecision::NewPossibleDuplicate(ids) => {
                    proptest::prop_assert!(ids.len() >= 2);
                    for id in ids {
                        let listed = candidates.iter().find(|c| c.transaction == id);
                        proptest::prop_assert!(listed.is_some_and(|c| is_eligible(&record, c)));
                    }
                }
                MatchDecision::New => {
                    proptest::prop_assert!(
                        !candidates.iter().any(|c| is_eligible(&record, c))
                    );
                }
            }
        }
    }
}
