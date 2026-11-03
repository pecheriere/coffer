//! What a forecast reads besides goals: its balances, items, this month's spending,
//! loose ends, spending newer than a balance, and connections.

use coffer_domain::accounts::{AccountKind, ConnectionLabel, LiquidityClass};
use coffer_domain::ids::{AccountId, CategoryId, MemberId, PlannedItemId};
use coffer_domain::money::Currency;
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_store::{
    AccountChanges, ConnectionHealth, ConnectionStatus, LooseEndsRow, NewAccount, NewConnection,
    NewPlannedItem,
};
use jiff::civil::{Date, date};

use super::super::plan::{at, one_off, world};
use super::{AS_OF, Flag, cash_account};

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

/// What can pay a bill: each transaction, by date, back to the first of last month, and no
/// further; a refund shows as one; a removed charge and a newer one are out, as they are of the
/// category's spending; and a possible duplicate says so.
#[tokio::test]
async fn spent_transactions_reach_back_a_month_one_by_one() {
    let world = world().await;
    let balance_on = date(2026, 11, 10);
    world.balance(world.account, balance_on, 0).await;
    world.spend(world.account, date(2026, 9, 30), -1).await;
    world.spend(world.account, date(2026, 10, 1), -10).await;
    world.spend(world.account, date(2026, 11, 2), 200).await;
    world.spend(world.account, date(2026, 11, 3), -3_000).await;
    world.flag(Flag::SourceRemoved, -3_000).await;
    world.spend(world.account, date(2026, 11, 4), -40_000).await;
    world.flag(Flag::PossibleDuplicate, -40_000).await;
    world
        .spend(world.account, date(2026, 11, 11), -500_000)
        .await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let rows: Vec<(Date, i64, bool)> = data
        .spent_transactions
        .iter()
        .map(|row| (row.posted_on, row.spent_minor, row.possible_duplicate))
        .collect();
    assert_eq!(
        rows,
        [
            (date(2026, 10, 1), 10, false),
            (date(2026, 11, 2), -200, false),
            (date(2026, 11, 4), 40_000, true),
        ]
    );
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
    let income = world.category_of(CategoryKind::Income).await;
    let transfer = world.category_of(CategoryKind::Transfer).await;
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
        .transaction(world.account, on, -32, Some((transfer, None)), false)
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

#[tokio::test]
async fn category_spent_leaves_out_removed_and_revised_charges() {
    let world = world().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    world.spend(world.account, on, -1).await;
    world.spend(world.account, on, -20).await;
    world.spend(world.account, on, -300).await;
    world.flag(Flag::SourceRemoved, -20).await;
    world.flag(Flag::SplitMismatch, -300).await;

    assert_eq!(world.category_spent().await, [1]);
}

/// A flag takes a charge out, which errs toward holding more. Taking a refund out would
/// raise the spending, so a flagged inflow keeps counting: 1,000 - 100 - 20.
#[tokio::test]
async fn category_spent_keeps_flagged_refunds() {
    let world = world().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    world.spend(world.account, on, -1_000).await;
    world.spend(world.account, on, 100).await;
    world.spend(world.account, on, 20).await;
    world.flag(Flag::SourceRemoved, 100).await;
    world.flag(Flag::SplitMismatch, 20).await;

    assert_eq!(world.category_spent().await, [880]);
}

/// The flag is the transaction's and the rule is per split: of one with mixed splits, the
/// outflow goes and the inflow stays.
#[tokio::test]
async fn category_spent_applies_a_flag_split_by_split() {
    let world = world().await;
    let other = world.category_of(CategoryKind::Expense).await;
    world.balance(world.account, AS_OF, 0).await;
    world.spend(world.account, date(2026, 11, 2), -1_000).await;
    world
        .split_transaction(
            world.account,
            date(2026, 11, 3),
            "USD",
            &[
                SplitInput {
                    category: world.category,
                    minor_units: 200,
                    goal: None,
                },
                SplitInput {
                    category: other,
                    minor_units: -500,
                    goal: None,
                },
            ],
        )
        .await;
    world.flag(Flag::SplitMismatch, -300).await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let spent_in = |category: CategoryId| {
        data.category_spent
            .iter()
            .find(|row| row.category == category)
            .map(|row| row.spent_minor)
    };
    assert_eq!(spent_in(world.category), Some(800), "the refund counts");
    assert_eq!(spent_in(other), None, "the charge doesn't");
}

#[tokio::test]
async fn spending_on_a_short_term_liability_counts() {
    let world = world().await;
    let card = world
        .forecast_account(NewAccount {
            kind: AccountKind::CreditCard,
            liquidity: LiquidityClass::ShortTermLiability,
            ..cash_account("Card")
        })
        .await;
    world.balance(card, AS_OF, -500).await;
    world.spend(card, date(2026, 11, 2), -500).await;

    assert_eq!(world.category_spent().await, [500]);
}

#[tokio::test]
async fn a_split_across_categories_counts_in_each() {
    let world = world().await;
    let other = world.category_of(CategoryKind::Expense).await;
    world.balance(world.account, AS_OF, 0).await;
    world
        .split_transaction(
            world.account,
            date(2026, 11, 2),
            "USD",
            &[
                SplitInput {
                    category: world.category,
                    minor_units: -60,
                    goal: None,
                },
                SplitInput {
                    category: other,
                    minor_units: -40,
                    goal: None,
                },
            ],
        )
        .await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let spent_in = |category: CategoryId| {
        data.category_spent
            .iter()
            .find(|row| row.category == category)
            .map(|row| row.spent_minor)
    };
    assert_eq!(spent_in(world.category), Some(60));
    assert_eq!(spent_in(other), Some(40));
}

#[tokio::test]
async fn category_spent_groups_by_currency() {
    let world = world().await;
    let euros = world
        .forecast_account(NewAccount {
            currency: Currency::parse("EUR").unwrap(),
            ..cash_account("Euros")
        })
        .await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    world.balance(euros, AS_OF, 0).await;
    let split = |minor_units| SplitInput {
        category: world.category,
        minor_units,
        goal: None,
    };
    world
        .split_transaction(world.account, on, "USD", &[split(-1)])
        .await;
    world
        .split_transaction(euros, on, "EUR", &[split(-20)])
        .await;

    let data = world.store.forecast_data(AS_OF).await.unwrap();

    let rows: Vec<(&str, i64)> = data
        .category_spent
        .iter()
        .map(|row| (row.currency.as_str(), row.spent_minor))
        .collect();
    assert_eq!(rows, [("EUR", 20), ("USD", 1)], "never netted");
}

/// Pending outflows count on any date up to `as_of`, before the balance's date too, and
/// only in `pending`: a transaction is one or the other.
#[tokio::test]
async fn newer_spending_totals_pending_outflows_apart() {
    let world = world().await;
    let account = world.account;
    world.balance(account, date(2026, 11, 10), 0).await;
    for (on, minor) in [
        (date(2026, 11, 2), -1),
        (date(2026, 11, 10), -20),
        (date(2026, 11, 12), -300),
        (AS_OF, -4_000),
        (date(2026, 11, 16), -50_000),
        (date(2026, 11, 12), 600_000),
    ] {
        world.transaction(account, on, minor, None, true).await;
    }

    assert_eq!(
        world.newer_spending().await,
        [("USD".to_owned(), 0, 4_321)],
        "nothing posted, every pending outflow by as_of"
    );
}

/// Outflows only, posted after the balance's date and by `as_of`, categorized or not.
#[tokio::test]
async fn newer_spending_totals_posted_outflows_after_the_balance() {
    let world = world().await;
    let account = world.account;
    world.balance(account, date(2026, 11, 10), 0).await;
    world
        .transaction(account, date(2026, 11, 10), -1, None, false)
        .await;
    world
        .transaction(account, date(2026, 11, 11), -20, None, false)
        .await;
    world.spend(account, AS_OF, -300).await;
    world
        .transaction(account, date(2026, 11, 16), -4_000, None, false)
        .await;
    world
        .transaction(account, date(2026, 11, 12), -50_000, None, true)
        .await;
    world
        .transaction(account, date(2026, 11, 12), 600_000, None, false)
        .await;

    assert_eq!(
        world.newer_spending().await,
        [("USD".to_owned(), 320, 50_000)]
    );
}

/// Only what could be in the cash. From an account out of the forecast, of another class
/// or closed cash, it isn't. A closed card switched in may hold a debt its balance doesn't
/// show, with or without a balance. An open account with no balance blocks the forecast
/// already.
#[tokio::test]
async fn newer_spending_is_only_on_counted_accounts() {
    let world = world().await;
    let (balance_on, on) = (date(2026, 11, 10), date(2026, 11, 12));
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
    let card = |name: &'static str| NewAccount {
        kind: AccountKind::CreditCard,
        liquidity: LiquidityClass::ShortTermLiability,
        ..cash_account(name)
    };
    let owing = world.forecast_account(card("Owing")).await;
    let settled = world.forecast_account(card("Settled")).await;
    let unbalanced = world.forecast_account(card("Unbalanced")).await;
    let switched_out = world
        .forecast_account(NewAccount {
            include_in_forecast: false,
            ..card("Switched out")
        })
        .await;
    let closed_cash = world.forecast_account(cash_account("Closed cash")).await;
    for (account, balance) in [
        (excluded, 0),
        (invested, 0),
        (owing, -100),
        (settled, 0),
        (switched_out, -100),
        (closed_cash, 0),
    ] {
        world.balance(account, balance_on, balance).await;
    }
    let mut tx = world.store.begin().await.unwrap();
    for account in [owing, settled, unbalanced, switched_out, closed_cash] {
        tx.update_account(
            account,
            AccountChanges {
                closed_at: Some(Some(at(5))),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    world.transaction(world.account, on, -1, None, false).await;
    world.transaction(excluded, on, -20, None, false).await;
    world.transaction(invested, on, -300, None, false).await;
    world.transaction(settled, on, -4_000, None, false).await;
    world.transaction(owing, on, -50_000, None, false).await;
    world
        .transaction(unbalanced, on, -600_000, None, false)
        .await;
    world
        .transaction(switched_out, on, -7_000_000, None, false)
        .await;
    world
        .transaction(closed_cash, on, -80_000_000, None, false)
        .await;

    assert_eq!(
        world.newer_spending().await,
        [("USD".to_owned(), 654_000, 0)],
        "closed cards switched in warn, whatever their balance; no open account without one"
    );

    world.balance(world.account, balance_on, 0).await;
    assert_eq!(
        world.newer_spending().await,
        [("USD".to_owned(), 654_001, 0)],
        "with a balance, the open one counts as well"
    );
}

#[tokio::test]
async fn newer_spending_groups_by_currency() {
    let world = world().await;
    let euros = world
        .forecast_account(NewAccount {
            currency: Currency::parse("EUR").unwrap(),
            ..cash_account("Euros")
        })
        .await;
    let (balance_on, on) = (date(2026, 11, 10), date(2026, 11, 12));
    world.balance(world.account, balance_on, 0).await;
    world.balance(euros, balance_on, 0).await;
    let split = |minor_units| SplitInput {
        category: world.category,
        minor_units,
        goal: None,
    };
    world
        .split_transaction(world.account, on, "USD", &[split(-1)])
        .await;
    world
        .split_transaction(euros, on, "EUR", &[split(-20)])
        .await;

    assert_eq!(
        world.newer_spending().await,
        [("EUR".to_owned(), 20, 0), ("USD".to_owned(), 1, 0)],
        "never netted"
    );
}
