//! Plan history in the store (money redesign §10.3): migration 0032 turns each item's amount,
//! direction, category, spending kind and tier into a first version without losing a
//! settlement or a goal link, and a version can only be written from this month or next.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::ids::{CategoryId, MemberId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{
    CalendarMonth, EffectiveFrom, PlanDirection, PlanSpending, PlanTier, PlanValues,
};
use coffer_store::{Store, StoreError};
use jiff::civil::date;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};

use crate::plan::{World, one_off, read_month, world};
use crate::plan_tiers::migrations_up_to;

fn month(text: &str) -> CalendarMonth {
    text.parse().unwrap()
}

fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("USD").unwrap(),
    }
}

/// The ids of the items seeded at 0031.
struct Seeded {
    bill: PlannedItemId,
    budget: PlannedItemId,
    paused_one_off: PlannedItemId,
    goal: PlannedItemId,
    income: PlannedItemId,
}

/// Every kind of item, a paused one, a settlement and a split linked to the goal, as a
/// database at 0031 holds them. The items were created on 2026-09-21.
fn seed(seeded: &Seeded, category: CategoryId, member: MemberId) -> String {
    format!(
        "
INSERT INTO members (id, username, display_name, role, created_at)
    VALUES ('{member}', 'alice', '', 'admin', 0);
INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden,
        include_in_net_worth, include_in_forecast, closed_at, created_at)
    VALUES ('019a0000-0000-7000-8000-000000000001', 'Checking', 'checking', 'USD', 'cash', 0,
        1, 1, NULL, 1);
INSERT INTO category_groups (id, name, position, created_at) VALUES ('group', 'Home', 0, 1);
INSERT INTO categories (id, group_id, name, kind, position, created_at)
    VALUES ('{category}', 'group', 'Rent', 'expense', 0, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction,
        account_hint_id, notes, active, item_type, every, unit, anchor_on, end_on, spending,
        tier, created_at, updated_at) VALUES
    ('{bill}', 'Rent', '{category}', 150000, 'USD', 'outflow', '019a0000-0000-7000-8000-000000000001', 'the flat', 1,
        'recurring', 1, 'months', '2026-03-05', '2027-12-31', 'bill', 'must_pay',
        1790000000000, 1790000000001),
    ('{budget}', 'Groceries', '{category}', 40000, 'EUR', 'outflow', NULL, NULL, 1,
        'recurring', 2, 'weeks', '2026-02-10', NULL, 'budget', 'flexible', 1790000000000,
        1790000000001),
    ('{income}', 'Pay', '{category}', 300000, 'USD', 'inflow', NULL, NULL, 1, 'recurring', 1,
        'months', '2025-12-28', NULL, NULL, NULL, 1790000000000, 1790000000001);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, one_off_on, tier, created_at, updated_at) VALUES
    ('{paused}', 'Repair', '{category}', 9900, 'USD', 'outflow', 0, 'one_off', '2026-11-01',
        'essentials', 1790000000000, 1790000000001);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, goal_start_on, goal_due_on, goal_completed, tier, created_at, updated_at)
    VALUES ('{goal}', 'Holiday', '{category}', 120000, 'USD', 'outflow', 1, 'goal',
        '2026-01-15', '2027-01-01', 0, 'saving_toward', 1790000000000, 1790000000001);
INSERT INTO planned_item_settlements (item_id, month, kind, settled_by_member, settled_at)
    VALUES ('{bill}', '2026-02', 'paid', '{member}', 1), ('{bill}', '2026-03', 'skipped',
        '{member}', 2), ('{paused}', '2026-11', 'skipped', '{member}', 3);
INSERT INTO transactions (id, account_id, origin, posted_on, pending, amount_minor, currency,
        raw_description, payee_edited, review, flag_possible_duplicate, flag_source_removed,
        flag_split_mismatch, created_at, updated_at) VALUES
    ('txn', '019a0000-0000-7000-8000-000000000001', 'manual', '2026-10-01', 0, -500, 'USD', 'Hotel', 0, 'unreviewed', 0,
        0, 0, 1, 1);
INSERT INTO transaction_splits (id, transaction_id, category_id, minor_units, position, goal_id)
    VALUES ('split', 'txn', '{category}', -500, 0, '{goal}');
",
        bill = seeded.bill,
        budget = seeded.budget,
        paused = seeded.paused_one_off,
        goal = seeded.goal,
        income = seeded.income,
    )
}

#[tokio::test]
async fn migration_versions_every_item_and_keeps_settlements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let seeded = Seeded {
        bill: PlannedItemId::new(),
        budget: PlannedItemId::new(),
        paused_one_off: PlannedItemId::new(),
        goal: PlannedItemId::new(),
        income: PlannedItemId::new(),
    };
    let category = CategoryId::new();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    migrations_up_to(31).run(&pool).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(seed(
        &seeded,
        category,
        MemberId::new(),
    )))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = Store::open(&path).await.unwrap();

    check_first_versions(&store, &seeded, category).await;
    check_links_and_keys(&path, &seeded).await;
}

/// Every item has one version equal to its old values, and what stayed with the item stayed.
async fn check_first_versions(store: &Store, seeded: &Seeded, category: CategoryId) {
    let eur = Money {
        minor_units: 40_000,
        currency: Currency::parse("EUR").unwrap(),
    };
    // (item, first month, amount, direction, spending, tier)
    for (id, from, amount, direction, spending, tier) in [
        (
            seeded.bill,
            "2026-03",
            usd(150_000),
            PlanDirection::Outflow,
            Some(PlanSpending::Bill),
            Some(PlanTier::MustPay),
        ),
        (
            seeded.budget,
            "2026-02",
            eur,
            PlanDirection::Outflow,
            Some(PlanSpending::Budget),
            Some(PlanTier::Flexible),
        ),
        (
            seeded.income,
            "2025-12",
            usd(300_000),
            PlanDirection::Inflow,
            None,
            None,
        ),
        (
            seeded.paused_one_off,
            "2026-11",
            usd(9_900),
            PlanDirection::Outflow,
            None,
            Some(PlanTier::Essentials),
        ),
        (
            seeded.goal,
            "2026-01",
            usd(120_000),
            PlanDirection::Outflow,
            None,
            Some(PlanTier::SavingToward),
        ),
    ] {
        let record = store.get_planned_item(id, read_month()).await.unwrap();
        let [only] = record.versions.rows() else {
            panic!("one version each");
        };
        assert_eq!(only.seq, 1);
        assert_eq!(only.effective_from, month(from), "from the anchor's month");
        assert_eq!(
            only.written_on,
            date(2026, 9, 21),
            "written the day it was created"
        );
        assert_eq!(only.values.category, category);
        assert_eq!(
            (amount, direction, spending, tier),
            (
                only.values.amount,
                only.values.direction,
                only.values.spending,
                only.values.tier
            )
        );
        assert_eq!(record.updated_at.as_millisecond(), 1_790_000_000_001);
    }
    let rent = store
        .get_planned_item(seeded.bill, read_month())
        .await
        .unwrap();
    assert_eq!(rent.notes.unwrap().as_str(), "the flat");
    assert!(rent.account_hint.is_some(), "the account hint is kept");
    assert!(
        !store
            .get_planned_item(seeded.paused_one_off, read_month())
            .await
            .unwrap()
            .active,
        "a paused item stays paused"
    );
    assert_eq!(
        store.settlements_of_item(seeded.bill).await.unwrap().len(),
        2
    );
    assert_eq!(
        store
            .settlements_of_item(seeded.paused_one_off)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// The goal's linked split and every foreign key survive the rebuild.
async fn check_links_and_keys(path: &std::path::Path, seeded: &Seeded) {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(path)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    let linked: String = sqlx::query("SELECT goal_id FROM transaction_splits WHERE id = 'split'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("goal_id");
    assert_eq!(linked, seeded.goal.to_string());
    let broken = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(broken.is_empty(), "no foreign key is left dangling");
    pool.close().await;
}

fn bill_values(world: &World, amount: i64) -> PlanValues {
    PlanValues {
        amount: usd(amount),
        direction: PlanDirection::Outflow,
        category: world.category,
        spending: Some(PlanSpending::Bill),
        tier: Some(PlanTier::MustPay),
    }
}

fn recurring() -> coffer_domain::plan::PlanSchedule {
    coffer_domain::plan::PlanSchedule::Recurring {
        every: 1,
        unit: coffer_domain::plan::IntervalUnit::Months,
        anchor_on: date(2027, 1, 5),
        end_on: None,
    }
}

#[tokio::test]
async fn a_version_from_next_month_leaves_this_month() {
    let world = world().await;
    let id = world.insert(world.new_item(recurring())).await;
    let today = date(2027, 6, 10);

    let mut tx = world.store.begin().await.unwrap();
    let written = tx
        .add_plan_version(
            id,
            today,
            EffectiveFrom::NextMonth,
            bill_values(&world, 5_000),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        (written.seq, written.effective_from, written.written_on),
        (2, month("2027-07"), today)
    );
    let this = world
        .store
        .get_planned_item(id, month("2027-06"))
        .await
        .unwrap();
    let next = world
        .store
        .get_planned_item(id, month("2027-07"))
        .await
        .unwrap();
    assert_eq!(this.amount, usd(4_200));
    assert_eq!(next.amount, usd(5_000));
    assert_eq!(next.versions.rows().len(), 2);
}

#[tokio::test]
async fn no_month_outside_this_and_next_can_be_written() {
    let world = world().await;
    let id = world.insert(world.new_item(recurring())).await;
    let one_off = world.insert(world.new_item(one_off())).await;
    let today = date(2027, 6, 10);

    let mut tx = world.store.begin().await.unwrap();
    for (item, from) in [
        (id, EffectiveFrom::Month(month("2027-05"))),
        (id, EffectiveFrom::Month(month("2027-08"))),
        (one_off, EffectiveFrom::NextMonth),
    ] {
        let refused = tx
            .add_plan_version(item, today, from, bill_values(&world, 1))
            .await;
        assert!(
            matches!(refused, Err(StoreError::Invalid(_))),
            "{from}: {refused:?}"
        );
    }
    let missing = tx
        .add_plan_version(
            PlannedItemId::new(),
            today,
            EffectiveFrom::ThisMonth,
            bill_values(&world, 1),
        )
        .await;
    assert!(matches!(missing, Err(StoreError::NotFound)));
}

#[tokio::test]
async fn the_check_refuses_an_inflow_with_a_tier() {
    let world = world().await;
    let id = world.insert(world.new_item(one_off())).await;
    let pool = world.pool().await;

    let refused = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO planned_item_versions (item_id, seq, effective_from, written_on, \
         amount_minor, currency, direction, category_id, tier) \
         VALUES ('{id}', 2, '2027-06', '2027-06-01', 100, 'USD', 'inflow', '{category}', \
         'flexible')",
        category = world.category,
    )))
    .execute(&pool)
    .await;

    assert!(refused.is_err());
    pool.close().await;
}

#[tokio::test]
async fn deleting_an_item_deletes_its_versions() {
    let world = world().await;
    let id = world.insert(world.new_item(recurring())).await;
    let mut tx = world.store.begin().await.unwrap();
    tx.add_plan_version(
        id,
        date(2027, 6, 10),
        EffectiveFrom::NextMonth,
        bill_values(&world, 1),
    )
    .await
    .unwrap();
    tx.delete_planned_item(id).await.unwrap();
    assert!(!tx.category_has_planned_items(world.category).await.unwrap());
    tx.commit().await.unwrap();

    let pool = world.pool().await;
    let left: i64 = sqlx::query("SELECT count(*) AS n FROM planned_item_versions")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("n");
    assert_eq!(left, 0);
    pool.close().await;
}
