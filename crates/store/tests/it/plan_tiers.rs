//! Tiers on planned items (money redesign §4.1): migration 0031 gives every stored outflow
//! the default for its kind and keeps settlements, and the store refuses an outflow read
//! without one; since 0032 the versions table's CHECK refuses an outflow without a tier.

use std::borrow::Cow;

use coffer_domain::ids::{CategoryId, MemberId, PlannedItemId};
use coffer_domain::plan::{EffectiveFrom, PlanSchedule, PlanTier, PlanValues, PlannedItemName};
use coffer_store::{PlannedItemChanges, Store};
use jiff::civil::date;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;

use crate::plan::{at, one_off, read_month, world};

pub(crate) fn migrations_up_to(last: i64) -> Migrator {
    let mut upto = sqlx::migrate!("./migrations");
    upto.migrations = Cow::Owned(
        upto.iter()
            .filter(|migration| migration.version <= last)
            .cloned()
            .collect(),
    );
    upto
}

/// The ids of the items seeded at 0030.
struct Seeded {
    bill: PlannedItemId,
    budget: PlannedItemId,
    one_off: PlannedItemId,
    goal: PlannedItemId,
    income: PlannedItemId,
}

/// A bill with a settlement, a budget, a one-off, a goal and an income item, seeded on a
/// database at 0030.
fn seed(seeded: &Seeded, category: CategoryId, member: MemberId) -> String {
    format!(
        "
INSERT INTO members (id, username, display_name, role, created_at)
    VALUES ('{member}', 'alice', '', 'admin', 0);
INSERT INTO category_groups (id, name, position, created_at) VALUES ('group', 'Home', 0, 1);
INSERT INTO categories (id, group_id, name, kind, position, created_at)
    VALUES ('{category}', 'group', 'Rent', 'expense', 0, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, every, unit, anchor_on, spending, created_at, updated_at) VALUES
    ('{bill}', 'Rent', '{category}', 100, 'USD', 'outflow', 1, 'recurring', 1, 'months',
        '2026-01-01', 'bill', 1, 1),
    ('{budget}', 'Groceries', '{category}', 100, 'USD', 'outflow', 1, 'recurring', 1,
        'months', '2026-01-01', 'budget', 1, 1),
    ('{income}', 'Pay', '{category}', 100, 'USD', 'inflow', 1, 'recurring', 1, 'months',
        '2026-01-01', NULL, 1, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, one_off_on, created_at, updated_at) VALUES
    ('{one_off}', 'Repair', '{category}', 100, 'USD', 'outflow', 1, 'one_off', '2026-03-01',
        1, 1);
INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, active,
        item_type, goal_start_on, goal_due_on, goal_completed, created_at, updated_at) VALUES
    ('{goal}', 'Holiday', '{category}', 100, 'USD', 'outflow', 1, 'goal', '2026-01-01',
        '2027-01-01', 0, 1, 1);
INSERT INTO planned_item_settlements (item_id, month, kind, settled_by_member, settled_at)
    VALUES ('{bill}', '2026-02', 'paid', '{member}', 1);
",
        bill = seeded.bill,
        budget = seeded.budget,
        one_off = seeded.one_off,
        goal = seeded.goal,
        income = seeded.income,
    )
}

#[tokio::test]
async fn migration_tiers_every_outflow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let seeded = Seeded {
        bill: PlannedItemId::new(),
        budget: PlannedItemId::new(),
        one_off: PlannedItemId::new(),
        goal: PlannedItemId::new(),
        income: PlannedItemId::new(),
    };
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    migrations_up_to(30).run(&pool).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(seed(
        &seeded,
        CategoryId::new(),
        MemberId::new(),
    )))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = Store::open(&path).await.unwrap();

    for (id, expected, what) in [
        (seeded.bill, Some(PlanTier::MustPay), "a bill is must pay"),
        (
            seeded.budget,
            Some(PlanTier::Flexible),
            "a budget is flexible",
        ),
        (
            seeded.one_off,
            Some(PlanTier::MustPay),
            "a one-off is must pay",
        ),
        (
            seeded.goal,
            Some(PlanTier::SavingToward),
            "a goal is saving toward",
        ),
        (seeded.income, None, "income has none"),
    ] {
        let record = store.get_planned_item(id, read_month()).await.unwrap();
        assert_eq!(record.tier, expected, "{what}");
    }
    let settlements = store.settlements_of_item(seeded.bill).await.unwrap();
    assert_eq!(settlements.len(), 1, "the bill's settlement is kept");
}

#[tokio::test]
async fn the_check_refuses_a_tier_that_breaks_the_rules() {
    let world = world().await;
    let bill = world.insert(world.new_item(one_off())).await;
    let pool = world.pool().await;
    let insert = |direction: &str, tier: &str| {
        format!(
            "INSERT INTO planned_item_versions (item_id, seq, effective_from, written_on, \
             amount_minor, currency, direction, category_id, spending, tier) VALUES ('{bill}', 2, \
             '2027-03', '2027-03-01', 100, 'USD', '{direction}', '{category}', NULL, {tier})",
            category = world.category,
        )
    };

    for (sql, what) in [
        (insert("outflow", "'emergency_fund'"), "an unknown tier"),
        (insert("inflow", "'essentials'"), "income keeping a tier"),
        (insert("outflow", "NULL"), "an outflow without a tier"),
    ] {
        assert!(
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&pool)
                .await
                .is_err(),
            "the schema refuses {what}"
        );
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(insert("inflow", "NULL")))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn tier_roundtrips_and_changes() {
    let world = world().await;
    let mut new = world.new_item(one_off());
    new.tier = Some(PlanTier::Essentials);
    let id = world.insert(new).await;
    let read = world
        .store
        .get_planned_item(id, read_month())
        .await
        .unwrap();
    assert_eq!(read.tier, Some(PlanTier::Essentials), "the tier is stored");

    let mut tx = world.store.begin().await.unwrap();
    tx.add_plan_version(
        id,
        date(2027, 6, 10),
        EffectiveFrom::ThisMonth,
        PlanValues {
            tier: Some(PlanTier::Flexible),
            ..read.versions.in_force(read_month()).to_owned()
        },
    )
    .await
    .unwrap();
    let record = tx.get_planned_item(id, read_month()).await.unwrap();
    assert_eq!(record.tier, Some(PlanTier::Flexible), "the tier changes");
    tx.update_planned_item(
        id,
        PlannedItemChanges {
            name: Some(PlannedItemName::parse("Renamed").unwrap()),
            ..PlannedItemChanges::default()
        },
        at(4),
    )
    .await
    .unwrap();
    let record = tx.get_planned_item(id, read_month()).await.unwrap();
    assert_eq!(record.tier, Some(PlanTier::Flexible), "a rename keeps it");
}

/// The triggers refuse a tier that doesn't fit the item's type; were one written anyway, the
/// store refuses what reads back wrong.
#[tokio::test]
async fn a_tier_that_does_not_fit_the_item_is_corrupt() {
    let world = world().await;
    let goal = world
        .insert(world.new_item(PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2027, 7, 1),
            completed: false,
        }))
        .await;
    let bill = world.insert(world.new_item(one_off())).await;
    world.allow_corrupting_versions().await;
    let pool = world.pool().await;

    for (item, tier) in [(goal, "flexible"), (bill, "saving_toward")] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "UPDATE planned_item_versions SET tier = '{tier}' WHERE item_id = '{item}'"
        )))
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            world.store.get_planned_item(item, read_month()).await,
            Err(coffer_store::StoreError::Backend(_))
        ));
    }
    assert!(matches!(
        world.store.list_planned_items(true, read_month()).await,
        Err(coffer_store::StoreError::Backend(_))
    ));
    pool.close().await;
}
