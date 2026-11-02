//! What lowers a goal's need: spending its balance shows, and nothing moved, removed,
//! revised or possibly counted twice.

use coffer_domain::accounts::{AccountKind, LiquidityClass};
use coffer_domain::transactions::CategoryKind;
use coffer_store::{AccountChanges, NewAccount};
use jiff::civil::date;

use super::super::plan::{at, world};
use super::{AS_OF, Flag, cash_account};

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

/// A closed card switched in may hold a debt no balance shows yet, so on all of them a
/// purchase newer than the balance doesn't lower the need: owing, settled, in credit or
/// with no balance at all. One on or before the balance's date is shown, and does.
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
    let (before, after) = (date(2026, 11, 10), date(2026, 11, 14));

    // The purchase is newer than the balance on all of them, so none shows it: owing,
    // settled at 0, in credit, or with no balance at all.
    let owing = closed_card("Owing", Some(-100)).await;
    world.spend_on_goal(owing, after, goal, -1).await;
    assert_eq!(world.goal_spent(goal).await, 0, "owing: limited");
    let settled = closed_card("Settled", Some(0)).await;
    world.spend_on_goal(settled, after, goal, -2).await;
    assert_eq!(world.goal_spent(goal).await, 0, "settled: limited");
    let credit = closed_card("Credit", Some(50)).await;
    world.spend_on_goal(credit, after, goal, -4).await;
    assert_eq!(world.goal_spent(goal).await, 0, "in credit: limited");
    let unbalanced = closed_card("Unbalanced", None).await;
    world.spend_on_goal(unbalanced, after, goal, -8).await;
    assert_eq!(world.goal_spent(goal).await, 0, "no balance: limited");

    // On or before the balance's date it shows what was spent.
    world.spend_on_goal(settled, before, goal, -300).await;
    assert_eq!(world.goal_spent(goal).await, 300, "shown by the balance");
}

/// A closed cash account was never spending counted cash on credit: nothing about it
/// is owed later, so no balance date holds its goal spending back.
#[tokio::test]
async fn goal_spent_on_a_closed_cash_account_has_no_balance_limit() {
    let world = world().await;
    let goal = world.goal().await;
    world
        .spend_on_goal(world.account, date(2026, 11, 14), goal, -300)
        .await;
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

    assert_eq!(world.goal_spent(goal).await, 300);
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

#[tokio::test]
async fn goal_spent_leaves_out_removed_revised_and_possibly_duplicated_charges() {
    let world = world().await;
    let goal = world.goal().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    for minor in [-1, -20, -300, -4_000] {
        world.spend_on_goal(world.account, on, goal, minor).await;
    }
    world.flag(Flag::SourceRemoved, -20).await;
    world.flag(Flag::SplitMismatch, -300).await;
    world.flag(Flag::PossibleDuplicate, -4_000).await;

    assert_eq!(world.goal_spent(goal).await, 1);
}

/// A flag takes a charge out, which errs toward holding more. Taking a refund out would
/// lower the need, so a flagged inflow keeps counting: 1,000 - 100 - 20 - 3.
#[tokio::test]
async fn goal_spent_keeps_flagged_refunds() {
    let world = world().await;
    let goal = world.goal().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    world.spend_on_goal(world.account, on, goal, -1_000).await;
    for refund in [100, 20, 3] {
        world.spend_on_goal(world.account, on, goal, refund).await;
    }
    world.flag(Flag::SourceRemoved, 100).await;
    world.flag(Flag::SplitMismatch, 20).await;
    world.flag(Flag::PossibleDuplicate, 3).await;

    assert_eq!(world.goal_spent(goal).await, 877);
}

/// Moved money and pay aren't goal spending, whatever the category's kind used to be.
#[tokio::test]
async fn goal_spent_counts_only_expense_splits() {
    let world = world().await;
    let goal = world.goal().await;
    let on = date(2026, 11, 2);
    world.balance(world.account, AS_OF, 0).await;
    let transfer = world.category_of(CategoryKind::Transfer).await;
    let income = world.category_of(CategoryKind::Income).await;
    world.spend_on_goal(world.account, on, goal, -1).await;
    world
        .transaction(world.account, on, -20, Some((transfer, Some(goal))), false)
        .await;
    world
        .transaction(world.account, on, -300, Some((income, Some(goal))), false)
        .await;

    assert_eq!(world.goal_spent(goal).await, 1);
}
