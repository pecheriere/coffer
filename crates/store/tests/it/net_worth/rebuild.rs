//! The full rebuild, and the proptest holding the incremental paths to it.

use coffer_domain::accounts::BalanceSource;
use coffer_domain::fx::FxSource;
use coffer_domain::ids::{AccountId, ConnectionId};
use coffer_store::Store;
use jiff::ToSpan;
use jiff::civil::{Date, date};
use proptest::prelude::{Just, ProptestConfig, Strategy, prop_assert_eq, prop_oneof, proptest};
use proptest::{collection, sample};

use super::{
    at, batch, connection, cur, dump, fill, flag, fx, holding, kept, mark, observations, oct, open,
    rebuild, rows_of, show_in, snap, snapshot, state, written_days,
};

#[tokio::test]
async fn rebuild_clears_the_flag() {
    let (_dir, store) = open().await;
    assert!(state(&store).await.rebuild_needed);

    rebuild(&store, oct(3)).await;

    let after = state(&store).await;
    assert!(!after.rebuild_needed);
    assert_eq!(after.filled_through, Some(oct(3)));
}

#[tokio::test]
async fn rebuild_computes_every_account_from_its_first_snapshot() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    let late = holding(&store, "USD").await;
    let empty = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    snap(&store, one, oct(3), 300).await;
    snap(&store, late, oct(4), 400).await;

    assert_eq!(rebuild(&store, oct(5)).await, 7);

    let balances: Vec<_> = rows_of(&store, one)
        .await
        .iter()
        .map(|row| (row.on, row.balance_minor, row.balance_on))
        .collect();
    assert_eq!(
        balances,
        [
            (oct(1), 100, oct(1)),
            (oct(2), 100, oct(1)),
            (oct(3), 300, oct(3)),
            (oct(4), 300, oct(3)),
            (oct(5), 300, oct(3)),
        ]
    );
    assert_eq!(rows_of(&store, late).await[0].on, oct(4));
    assert_eq!(rows_of(&store, empty).await, []);
}

/// More rows than one insert takes, so its batches must add up.
#[tokio::test]
async fn rebuild_writes_more_than_a_batch() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, date(2025, 1, 1), 100).await;

    let through = date(2026, 12, 31);
    assert_eq!(rebuild(&store, through).await, 730);
    assert_eq!(rows_of(&store, one).await.last().unwrap().on, through);
}

#[tokio::test]
async fn rebuild_is_idempotent() {
    let (_dir, store) = open().await;
    let one = holding(&store, "EUR").await;
    snap(&store, one, oct(1), 100).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(5)).await;
    let first = dump(&store).await;
    assert_eq!(first.len(), 5);

    rebuild(&store, oct(5)).await;

    assert_eq!(dump(&store).await, first);
}

/// A row nothing explains, planted before the account's first snapshot.
#[tokio::test]
async fn rebuild_removes_what_no_source_holds() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(5), 100).await;
    rebuild(&store, oct(5)).await;
    store
        .execute_unchecked_for_tests(
            "INSERT INTO account_daily_values (account_id, on_date, balance_minor, balance_on, \
             balance_source, conversion) VALUES (?1, '2026-10-01', 1, '2026-10-01', 'manual', \
             'no_display_currency')",
            &one.to_string(),
        )
        .await
        .unwrap();
    assert_eq!(rows_of(&store, one).await.len(), 2);

    rebuild(&store, oct(5)).await;

    assert_eq!(rows_of(&store, one).await.len(), 1);
}

/// A rebuild recomputes everything, so the day it is given is the last the table
/// holds, earlier than before or not.
#[tokio::test]
async fn rebuild_resets_through_to_the_day_given() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;

    assert_eq!(rebuild(&store, oct(3)).await, 3);

    assert_eq!(state(&store).await.filled_through, Some(oct(3)));
    assert_eq!(rows_of(&store, one).await.last().unwrap().on, oct(3));
}

/// A clock that jumped ahead left its rows behind: a rebuild takes them away, so
/// later changes don't keep paying for them.
#[tokio::test]
async fn a_rebuild_undoes_a_forward_clock_jump() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(2)).await;
    fill(&store, date(2027, 1, 1)).await;

    rebuild(&store, oct(3)).await;

    assert_eq!(state(&store).await.filled_through, Some(oct(3)));
    assert_eq!(rows_of(&store, one).await.len(), 3);
    assert_eq!(
        fill(&store, oct(2)).await,
        0,
        "a fill still never moves back"
    );
    assert_eq!(state(&store).await.filled_through, Some(oct(3)));
}

#[tokio::test]
async fn fill_is_idempotent() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(2)).await;

    assert_eq!(fill(&store, oct(4)).await, 2);
    mark(&store, one).await;
    assert_eq!(fill(&store, oct(4)).await, 0);

    assert_eq!(written_days(&store, one).await, []);
    assert_eq!(state(&store).await.filled_through, Some(oct(4)));
}

#[tokio::test]
async fn fill_never_moves_back() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(5)).await;

    assert_eq!(fill(&store, oct(3)).await, 0);

    assert_eq!(state(&store).await.filled_through, Some(oct(5)));
    assert_eq!(rows_of(&store, one).await.len(), 5);
}

#[tokio::test]
async fn fill_writes_only_the_days_after_through() {
    let (_dir, store) = open().await;
    let one = holding(&store, "USD").await;
    snap(&store, one, oct(1), 100).await;
    rebuild(&store, oct(2)).await;
    mark(&store, one).await;

    fill(&store, oct(4)).await;

    assert_eq!(written_days(&store, one).await, [oct(3), oct(4)]);
    assert_eq!(state(&store).await.filled_through, Some(oct(4)));
}

#[tokio::test]
async fn table_can_be_dropped_and_rebuilt_from_the_sources() {
    let (_dir, store) = open().await;
    let one = holding(&store, "JPY").await;
    snap(&store, one, oct(1), 100).await;
    fx(&store, "USD", &[(oct(1), "1.1")]).await;
    fx(&store, "JPY", &[(oct(2), "170")]).await;
    show_in(&store, "USD").await;
    rebuild(&store, oct(5)).await;
    let rows = dump(&store).await;
    assert_eq!(rows.len(), 5);

    store
        .execute_unchecked_for_tests(
            "DELETE FROM account_daily_values WHERE on_date >= ?",
            "0000-01-01",
        )
        .await
        .unwrap();
    store
        .execute_unchecked_for_tests(
            "UPDATE net_worth_state SET rebuild_needed = 1 WHERE filled_through = ?",
            &oct(5).to_string(),
        )
        .await
        .unwrap();
    assert_eq!(dump(&store).await, []);

    rebuild(&store, oct(5)).await;

    assert_eq!(dump(&store).await, rows);
    assert!(!state(&store).await.rebuild_needed);
}

/// One change to the sources. Indices pick among what exists when it runs.
#[derive(Debug, Clone)]
enum Op {
    Create(&'static str),
    Snapshot {
        account: usize,
        on: i8,
        minor: i64,
        manual: bool,
    },
    Rates {
        currency: &'static str,
        on: i8,
        rate: &'static str,
    },
    /// An answer about the days from `from` on, which may remove rates.
    Window {
        currency: &'static str,
        from: i8,
        answer: Vec<(i8, &'static str)>,
    },
    Display(&'static str),
    Fill(i8),
    /// A rebuild is asked for and gets this many batches of one account for now.
    SomeBatches(u8),
    Bind {
        account: usize,
        kept: Vec<(i8, i64, bool)>,
    },
}

/// The first day of the 40-day window, which spans the first day rows may have.
const START: Date = date(1969, 12, 20);

/// What stands for a day in the distant past, of no row at all.
const DISTANT: i8 = -1;

fn day(offset: i8) -> Date {
    if offset == DISTANT {
        return date(1, 1, 1);
    }
    START.checked_add(i64::from(offset).days()).unwrap()
}

/// A day of the window, or now and then one long before it.
fn offset() -> impl Strategy<Value = i8> {
    prop_oneof![9 => 0_i8..40, 1 => Just(DISTANT)]
}

fn code() -> impl Strategy<Value = &'static str> {
    sample::select(vec!["USD", "EUR", "JPY", "KWD"])
}

fn minor() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(100_i64),
        Just(250),
        Just(-75),
        Just(i64::MAX),
        0_i64..1_000_000,
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => code().prop_map(Op::Create),
        2 => (0_usize..8, offset(), minor(), proptest::bool::ANY).prop_map(
            |(account, on, minor, manual)| Op::Snapshot {
                account,
                on,
                minor,
                manual
            }
        ),
        3 => (
            sample::select(vec!["USD", "JPY", "KWD"]),
            offset(),
            sample::select(vec!["0.34", "1.1", "1.2", "170"])
        )
            .prop_map(|(currency, on, rate)| Op::Rates { currency, on, rate }),
        // Weighted, as a removal needs a rate stored first and a later answer that
        // passes it without holding it: at an equal weight it rarely moved a row.
        3 => (
            sample::select(vec!["USD", "JPY", "KWD"]),
            0_i8..20,
            collection::btree_map(0_i8..40, sample::select(vec!["1.1", "1.2", "170"]), 0..4),
            proptest::bool::weighted(0.7),
        )
            .prop_map(|(currency, from, mut answer, reaches_the_end)| {
                if reaches_the_end {
                    answer.insert(39, "1.1");
                }
                Op::Window {
                    currency,
                    from,
                    answer: answer.into_iter().collect(),
                }
            }),
        1 => code().prop_map(Op::Display),
        1 => (1_i8..10).prop_map(Op::Fill),
        2 => (0_u8..3).prop_map(Op::SomeBatches),
        1 => (
            0_usize..8,
            collection::vec((offset(), minor(), proptest::bool::weighted(0.8)), 0..5)
        )
            .prop_map(|(account, kept)| Op::Bind { account, kept }),
    ]
}

/// The household under test: its accounts by the currency each holds.
struct World {
    store: Store,
    bank: ConnectionId,
    accounts: Vec<(AccountId, &'static str)>,
    feeds: usize,
    through: Date,
}

impl World {
    fn pick(&self, index: usize) -> Option<(AccountId, &'static str)> {
        let count = self.accounts.len();
        (count > 0).then(|| self.accounts[index % count])
    }

    async fn apply(&mut self, change: Op) {
        match change {
            Op::Create(code) => {
                let account = holding(&self.store, code).await;
                self.accounts.push((account, code));
            }
            Op::Snapshot {
                account,
                on,
                minor,
                manual,
            } => {
                if let Some((account, _)) = self.pick(account) {
                    let source = if manual {
                        BalanceSource::Manual
                    } else {
                        BalanceSource::Sync
                    };
                    let mut tx = self.store.begin().await.unwrap();
                    tx.upsert_balance(snapshot(account, day(on), minor, source))
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                }
            }
            Op::Rates { currency, on, rate } => {
                let mut tx = self.store.begin().await.unwrap();
                tx.upsert_fx_rates(
                    cur(currency),
                    FxSource::Ecb,
                    &observations(&[(day(on), rate)]),
                    at(),
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            Op::Window {
                currency,
                from,
                answer,
            } => {
                let answer: Vec<_> = answer.iter().map(|&(on, rate)| (day(on), rate)).collect();
                let mut tx = self.store.begin().await.unwrap();
                tx.replace_fx_rates(
                    cur(currency),
                    FxSource::Ecb,
                    day(from),
                    &observations(&answer),
                    at(),
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            Op::Display(code) => show_in(&self.store, code).await,
            Op::Fill(more) => {
                self.through = self.through.checked_add(i64::from(more).days()).unwrap();
                fill(&self.store, self.through).await;
            }
            Op::SomeBatches(batches) => {
                if !state(&self.store).await.rebuild_needed {
                    flag(&self.store).await;
                }
                for _ in 0..batches {
                    batch(&self.store, self.through, 1).await;
                }
            }
            Op::Bind {
                account,
                kept: days,
            } => {
                let Some((account, code)) = self.pick(account) else {
                    return;
                };
                self.feeds += 1;
                let other = if code == "EUR" { "USD" } else { "EUR" };
                let entries: Vec<_> = days
                    .iter()
                    .map(|&(on, minor, same)| (day(on), minor, if same { code } else { other }))
                    .collect();
                let feed = kept(
                    &self.store,
                    self.bank,
                    &format!("feed-{}", self.feeds),
                    &entries,
                )
                .await;
                let mut tx = self.store.begin().await.unwrap();
                tx.bring_in_kept_balances(feed, account, cur(code))
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Each case opens and migrates a database of its own. 256 take about 5 s,
        // inside the 30 s limit even on a busy machine; 4,000 passed once, in 68 s.
        cases: 256,
        ..ProptestConfig::default()
    })]

    #[test]
    fn incremental_matches_a_full_rebuild(
        before in collection::vec(op(), 0..4),
        after in collection::vec(op(), 1..=12),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (incremental, full, through) = runtime.block_on(async {
            let (_dir, store) = open().await;
            let bank = connection(&store).await;
            let mut world = World {
                store,
                bank,
                accounts: Vec::new(),
                feeds: 0,
                through: day(20),
            };
            // What happens before the first rebuild is the migration's backlog.
            for change in before {
                world.apply(change).await;
            }
            rebuild(&world.store, world.through).await;
            for change in after {
                world.apply(change).await;
            }
            // A rebuild left running finishes, and the days it didn't reach are filled.
            world.store.finish_net_worth_rebuild(world.through).await.unwrap();
            fill(&world.store, world.through).await;
            let incremental = dump(&world.store).await;
            rebuild(&world.store, world.through).await;
            let full = dump(&world.store).await;
            let through = state(&world.store).await.filled_through;
            (incremental, full, (through, world.through))
        });
        prop_assert_eq!(incremental, full);
        prop_assert_eq!(through.0, Some(through.1));
    }
}
