//! What a forecast reads from the store (spec §8.9 R9, R10, R19): the balance it
//! takes, the spending that balance already shows, and an order that doesn't change.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{
    AccountKind, AccountName, BalanceSource, ConnectionLabel, LiquidityClass,
};
use coffer_domain::ids::{AccountId, CategoryId, MemberId, PlannedItemId};
use coffer_domain::money::Currency;
use coffer_domain::plan::PlanSchedule;
use coffer_domain::transactions::{
    CategoryGroupName, CategoryKind, CategoryName, Description, SplitInput, TransactionOrigin,
};
use coffer_store::{
    AccountChanges, Attribution, BalanceSnapshot, ConnectionHealth, ConnectionStatus, LooseEndsRow,
    NewAccount, NewCategory, NewCategoryGroup, NewConnection, NewPlannedItem, NewTransaction,
};
use jiff::civil::{Date, date};

use super::plan::{World, at, one_off, world};

const AS_OF: Date = date(2026, 11, 15);

/// How a test's account counts towards the forecast.
fn cash_account(name: &str) -> NewAccount {
    NewAccount {
        id: AccountId::new(),
        name: AccountName::parse(name).unwrap(),
        kind: AccountKind::Checking,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: at(0),
    }
}

impl World {
    async fn forecast_account(&self, new: NewAccount) -> AccountId {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx.insert_account(new).await.unwrap();
        tx.commit().await.unwrap();
        record.id
    }

    async fn balance(&self, account: AccountId, on: Date, balance_minor: i64) {
        let mut tx = self.store.begin().await.unwrap();
        tx.upsert_balance(BalanceSnapshot {
            account,
            on,
            balance_minor,
            source: BalanceSource::Manual,
            recorded_at: at(0),
        })
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// A transaction of `minor_units`, posted on `on`, with one split of all of it in
    /// `category`, or none without one.
    async fn transaction(
        &self,
        account: AccountId,
        on: Date,
        minor_units: i64,
        split: Option<(CategoryId, Option<PlannedItemId>)>,
        pending: bool,
    ) {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Manual,
                account,
                posted_on: on,
                transacted_on: None,
                pending,
                amount_minor: minor_units,
                currency: Currency::parse("USD").unwrap(),
                raw_description: Description::parse("Purchase").unwrap(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        if let Some((category, goal)) = split {
            tx.replace_splits(
                record.id,
                &[SplitInput {
                    category,
                    minor_units,
                    goal,
                }],
                Attribution {
                    member: None,
                    agent: None,
                    system: Some("test".into()),
                    at: at(0),
                },
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }

    async fn spend(&self, account: AccountId, on: Date, minor_units: i64) {
        let split = Some((self.category, None));
        self.transaction(account, on, minor_units, split, false)
            .await;
    }

    /// The goal `transaction` calls link to.
    async fn goal(&self) -> PlannedItemId {
        self.insert(self.new_item(PlanSchedule::Goal {
            start_on: date(2026, 1, 1),
            due_on: date(2027, 1, 1),
            completed: false,
        }))
        .await
    }

    async fn spend_on_goal(&self, account: AccountId, on: Date, goal: PlannedItemId, minor: i64) {
        let split = Some((self.category, Some(goal)));
        self.transaction(account, on, minor, split, false).await;
    }

    /// What `AS_OF`'s forecast reads as spent in the world's category.
    async fn category_spent(&self) -> Vec<i64> {
        let data = self.store.forecast_data(AS_OF).await.unwrap();
        data.category_spent
            .iter()
            .map(|row| row.spent_minor)
            .collect()
    }

    async fn goal_spent(&self, goal: PlannedItemId) -> i64 {
        let data = self.store.forecast_data(AS_OF).await.unwrap();
        data.goal_spent
            .iter()
            .find(|(found, _)| *found == goal)
            .map_or(0, |(_, spent)| *spent)
    }
}

#[tokio::test]
async fn balance_is_latest_on_or_before_as_of() {
    let world = world().await;
    let without = world.forecast_account(cash_account("Without")).await;
    world.balance(world.account, date(2026, 11, 1), 100).await;
    world.balance(world.account, AS_OF, 200).await;
    world.balance(world.account, date(2026, 11, 16), 300).await;
    world.balance(without, date(2026, 11, 16), 400).await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let balance_of = |account: AccountId| {
        data.accounts
            .iter()
            .find(|(record, _)| record.id == account)
            .unwrap()
            .1
            .as_ref()
            .map(|snapshot| (snapshot.on, snapshot.balance_minor))
    };
    assert!(
        balance_of(world.account) == Some((AS_OF, 200)),
        "the latest on or before as_of"
    );
    assert!(
        balance_of(without).is_none(),
        "only a future balance is none"
    );
}

#[tokio::test]
async fn forecast_data_lists_closed_accounts() {
    let world = world().await;
    let mut tx = world.store.begin().await.unwrap();
    tx.update_account(
        world.account,
        AccountChanges {
            closed_at: Some(Some(at(5))),
            ..AccountChanges::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    assert!(data.accounts.len() == 1, "closed ones are listed too");
}

#[tokio::test]
async fn category_spent_nets_refunds_and_floors_at_zero() {
    let world = world().await;
    world.balance(world.account, AS_OF, 0).await;
    world.spend(world.account, date(2026, 11, 2), -500).await;
    world.spend(world.account, date(2026, 11, 3), 200).await;
    assert_eq!(world.category_spent().await, [300], "netted");

    world.spend(world.account, date(2026, 11, 4), 700).await;
    assert_eq!(world.category_spent().await, [0], "floored at zero");
}

#[tokio::test]
async fn category_spent_counts_only_as_of_s_month() {
    let world = world().await;
    world.balance(world.account, AS_OF, 0).await;
    world.spend(world.account, date(2026, 10, 31), -1).await;
    world.spend(world.account, date(2026, 11, 1), -10).await;
    world.spend(world.account, AS_OF, -100).await;
    assert_eq!(world.category_spent().await, [110]);
}

#[tokio::test]
async fn category_spent_ignores_spending_newer_than_the_balance() {
    let world = world().await;
    world.balance(world.account, date(2026, 11, 10), 0).await;
    // A balance past `as_of` isn't the chosen one, so it can't widen the window.
    world.balance(world.account, date(2026, 11, 20), 0).await;
    let without_balance = world.forecast_account(cash_account("Without")).await;

    world.spend(world.account, date(2026, 11, 10), -100).await;
    world.spend(world.account, date(2026, 11, 11), -1_000).await;
    world
        .spend(without_balance, date(2026, 11, 2), -10_000)
        .await;

    assert_eq!(
        world.category_spent().await,
        [100],
        "the balance day counts; the day after, and no balance at all, don't"
    );
}

/// Balance dated Nov 10, a purchase of 500,000 posted Nov 14, `as_of` Nov 15: the
/// balance doesn't show it yet, so the need isn't reduced.
#[tokio::test]
async fn goal_spent_ignores_spending_newer_than_the_balance() {
    let world = world().await;
    let goal = world.goal().await;
    world.balance(world.account, date(2026, 11, 10), 0).await;
    world
        .spend_on_goal(world.account, date(2026, 11, 14), goal, -500_000)
        .await;
    assert_eq!(world.goal_spent(goal).await, 0, "not shown by the balance");

    world
        .spend_on_goal(world.account, date(2026, 11, 10), goal, -7)
        .await;
    assert_eq!(world.goal_spent(goal).await, 7, "the balance day shows it");
}

/// It never touched counted cash, so no balance date holds it back: from an account
/// switched out, of another class, or closed.
#[tokio::test]
async fn goal_spent_from_an_excluded_account_has_no_balance_limit() {
    let world = world().await;
    let goal = world.goal().await;
    let excluded = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..cash_account("Side")
        })
        .await;
    let invested = world
        .forecast_account(NewAccount {
            liquidity: LiquidityClass::Invested,
            ..cash_account("Brokerage")
        })
        .await;
    world.balance(excluded, date(2026, 11, 10), 0).await;
    world.balance(invested, date(2026, 11, 10), 0).await;
    let without_balance = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..cash_account("Unbalanced")
        })
        .await;

    let on = date(2026, 11, 14);
    world.spend_on_goal(excluded, on, goal, -500_000).await;
    world.spend_on_goal(invested, on, goal, -1_000).await;
    world.spend_on_goal(without_balance, on, goal, -10).await;

    assert_eq!(world.goal_spent(goal).await, 501_010);
}

/// No balance date limits it, so only `as_of` does.
#[tokio::test]
async fn goal_spent_from_an_excluded_account_stops_at_as_of() {
    let world = world().await;
    let goal = world.goal().await;
    let excluded = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..cash_account("Side")
        })
        .await;
    world.spend_on_goal(excluded, AS_OF, goal, -10).await;
    world
        .spend_on_goal(excluded, date(2026, 11, 16), goal, -500)
        .await;
    assert_eq!(world.goal_spent(goal).await, 10);
}

/// A counted account's balance can't show it, because none exists: the forecast is
/// blocked until one does, and then the balance's date decides.
#[tokio::test]
async fn goal_spent_on_a_counted_account_without_a_balance_counts_nothing() {
    let world = world().await;
    let goal = world.goal().await;
    world
        .spend_on_goal(world.account, date(2026, 11, 14), goal, -500)
        .await;
    assert_eq!(world.goal_spent(goal).await, 0);
}

/// A closed card still owing counts against cash (R16), so a purchase newer than its
/// balance isn't shown by it. One that no longer owes doesn't count, and a closed
/// account never touched counted cash.
#[tokio::test]
async fn goal_spent_on_a_closed_card_follows_whether_it_counts() {
    let world = world().await;
    let goal = world.goal().await;
    let closed_card = async |name: &str, balance: Option<i64>| {
        let card = world
            .forecast_account(NewAccount {
                kind: AccountKind::CreditCard,
                liquidity: LiquidityClass::ShortTermLiability,
                ..cash_account(name)
            })
            .await;
        if let Some(balance) = balance {
            world.balance(card, date(2026, 11, 10), balance).await;
        }
        let mut tx = world.store.begin().await.unwrap();
        tx.update_account(
            card,
            AccountChanges {
                closed_at: Some(Some(at(5))),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        card
    };
    let on = date(2026, 11, 14);

    let owing = closed_card("Owing", Some(-100)).await;
    world.spend_on_goal(owing, on, goal, -1).await;
    assert_eq!(world.goal_spent(goal).await, 0, "it counts, so limited");

    let settled = closed_card("Settled", Some(0)).await;
    world.spend_on_goal(settled, on, goal, -20).await;
    assert_eq!(world.goal_spent(goal).await, 20, "it doesn't count");

    let unbalanced = closed_card("Unbalanced", None).await;
    world.spend_on_goal(unbalanced, on, goal, -300).await;
    assert_eq!(world.goal_spent(goal).await, 320, "it doesn't count either");
}

#[tokio::test]
async fn goal_spent_nets_refunds_ignores_pending_and_the_future() {
    let world = world().await;
    let goal = world.goal().await;
    world.balance(world.account, AS_OF, 0).await;
    let split = Some((world.category, Some(goal)));
    let on = date(2026, 10, 1);

    world.spend_on_goal(world.account, on, goal, -500).await;
    world.spend_on_goal(world.account, on, goal, 200).await;
    world.transaction(world.account, on, -9, split, true).await;
    assert_eq!(
        world.goal_spent(goal).await,
        300,
        "netted, pending left out"
    );

    world
        .spend_on_goal(world.account, date(2026, 11, 16), goal, -40)
        .await;
    assert_eq!(world.goal_spent(goal).await, 300, "posted after as_of");

    world.spend_on_goal(world.account, on, goal, 5_000).await;
    assert_eq!(world.goal_spent(goal).await, 0, "floored at zero");
}

/// The table is scanned in rowid order, so ids rewritten out of step with it show
/// whether the query really orders them.
#[tokio::test]
async fn forecast_items_load_in_a_deterministic_order() {
    let world = world().await;
    let mut inserted = Vec::new();
    for _ in 0..3 {
        inserted.push(world.insert(world.new_item(one_off())).await);
    }
    let pool = world.pool().await;
    let rewritten = [
        "ffffffff-0000-7000-8000-000000000000",
        "00000000-0000-7000-8000-000000000000",
        "88888888-0000-7000-8000-000000000000",
    ];
    for (id, new_id) in inserted.iter().zip(rewritten) {
        sqlx::query("UPDATE planned_items SET id = ? WHERE id = ?")
            .bind(new_id)
            .bind(id.to_string())
            .execute(&pool)
            .await
            .unwrap();
    }

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let ids: Vec<String> = data.items.iter().map(|item| item.id.to_string()).collect();
    assert_eq!(
        ids,
        [rewritten[1], rewritten[2], rewritten[0]],
        "ordered by id, not by when they were stored"
    );
}

#[tokio::test]
async fn forecast_items_are_only_the_active_ones() {
    let world = world().await;
    let active = world.insert(world.new_item(one_off())).await;
    world
        .insert(NewPlannedItem {
            active: false,
            ..world.new_item(one_off())
        })
        .await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let ids: Vec<PlannedItemId> = data.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, [active]);
}

#[tokio::test]
async fn category_spent_leaves_out_what_does_not_count() {
    let world = world().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    let income = async {
        let mut tx = world.store.begin().await.unwrap();
        let group = tx
            .insert_category_group(NewCategoryGroup {
                name: CategoryGroupName::parse("Pay").unwrap(),
                position: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        let category = tx
            .insert_category(NewCategory {
                group: group.id,
                name: CategoryName::parse("Salary").unwrap(),
                kind: CategoryKind::Income,
                position: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        category.id
    }
    .await;
    let goal = world.goal().await;
    let excluded = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..cash_account("Side")
        })
        .await;
    let invested = world
        .forecast_account(NewAccount {
            liquidity: LiquidityClass::Invested,
            ..cash_account("Brokerage")
        })
        .await;
    world.balance(excluded, AS_OF, 0).await;
    world.balance(invested, AS_OF, 0).await;

    world
        .transaction(world.account, on, -1, Some((income, None)), false)
        .await;
    world
        .transaction(world.account, on, -2, Some((world.category, None)), true)
        .await;
    world.spend_on_goal(world.account, on, goal, -4).await;
    world.spend(excluded, on, -8).await;
    world.spend(invested, on, -16).await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    assert_eq!(data.category_spent, []);
}

/// One uncategorized outflow of 500 and one of 40 that count, and one flagged
/// possible duplicate. The rest is outside the window or the accounts that count.
#[tokio::test]
async fn loose_ends_count_uncategorized_outflows_and_duplicates() {
    let world = world().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, date(2026, 11, 10), 0).await;
    let excluded = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..cash_account("Side")
        })
        .await;
    world.balance(excluded, AS_OF, 0).await;

    let account = world.account;
    world.transaction(account, on, -500, None, false).await;
    world.transaction(account, on, -40, None, false).await;
    world.transaction(account, on, 9_000, None, false).await;
    world.spend(account, on, -70_000).await;
    world.transaction(account, on, -3, None, true).await;
    world
        .transaction(account, date(2026, 10, 31), -5, None, false)
        .await;
    world
        .transaction(account, date(2026, 11, 11), -6, None, false)
        .await;
    world.transaction(excluded, on, -7, None, false).await;
    let pool = world.pool().await;
    sqlx::query(
        "UPDATE transactions SET flag_possible_duplicate = 1 \
         WHERE amount_minor = -70000 OR amount_minor = -5",
    )
    .execute(&pool)
    .await
    .unwrap();

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    assert_eq!(
        data.loose_ends,
        [LooseEndsRow {
            currency: Currency::parse("USD").unwrap(),
            uncategorized_count: 2,
            uncategorized_minor: 540,
            possible_duplicates: 1,
        }]
    );
}

#[tokio::test]
async fn forecast_data_reads_connections_with_their_status() {
    let world = world().await;
    let member = MemberId::new();
    let pool = world.pool().await;
    sqlx::query(
        "INSERT INTO members (id, username, display_name, role, created_at) \
         VALUES (?, 'alice', '', 'admin', 0)",
    )
    .bind(member.to_string())
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = world.store.begin().await.unwrap();
    let connection = tx
        .insert_connection(NewConnection {
            provider: "simplefin",
            label: ConnectionLabel::parse("Family bank").unwrap(),
            owner: member,
            credentials: vec![1, 2, 3],
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    assert_eq!(
        data.connections,
        [ConnectionHealth {
            id: connection.id,
            label: "Family bank".into(),
            status: ConnectionStatus::Active,
        }]
    );
}
