//! What migration 0032's triggers keep of what the CHECKs of `planned_items` said before the values
//! moved to versions (a recurring outflow has a spending kind and nothing else does, a goal
//! is an outflow, only a goal saves toward), and that the versions are append-only.

use coffer_domain::ids::PlannedItemId;
use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, PlanTier};
use coffer_store::StoreError;
use jiff::civil::date;
use sqlx::SqlitePool;

use crate::plan::{World, one_off, world};

/// A second version of `item` with the given columns, as raw SQL.
async fn write_version(
    pool: &SqlitePool,
    world: &World,
    item: PlannedItemId,
    seq: u32,
    direction: &str,
    spending: &str,
    tier: &str,
) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO planned_item_versions (item_id, seq, effective_from, written_on, \
         amount_minor, currency, direction, category_id, spending, tier) VALUES ('{item}', {seq}, \
         '2027-02', '2027-01-15', 100, 'USD', '{direction}', '{category}', {spending}, {tier})",
        category = world.category,
    )))
    .execute(pool)
    .await
    .map(|_| ())
}

fn goal() -> PlanSchedule {
    PlanSchedule::Goal {
        start_on: date(2027, 1, 1),
        due_on: date(2027, 12, 1),
        completed: false,
    }
}

fn monthly() -> PlanSchedule {
    PlanSchedule::Recurring {
        every: 1,
        unit: IntervalUnit::Months,
        anchor_on: date(2027, 1, 1),
        end_on: None,
    }
}

#[tokio::test]
async fn the_database_refuses_a_version_that_does_not_fit_its_item() {
    let world = world().await;
    let pool = world.pool().await;
    let bill = world.insert(world.new_item(monthly())).await;
    let one_time = world.insert(world.new_item(one_off())).await;
    let saving = world.insert(world.new_item(goal())).await;
    let refused = [
        (
            bill,
            "'outflow'",
            "NULL",
            "'must_pay'",
            "a recurring outflow without a spending kind",
        ),
        (
            bill,
            "'inflow'",
            "'bill'",
            "NULL",
            "a recurring inflow with a spending kind",
        ),
        (
            bill,
            "'outflow'",
            "'bill'",
            "'saving_toward'",
            "saving toward on a bill",
        ),
        (
            one_time,
            "'outflow'",
            "'bill'",
            "'must_pay'",
            "a one-off with a spending kind",
        ),
        (
            one_time,
            "'outflow'",
            "NULL",
            "'saving_toward'",
            "saving toward on a one-off",
        ),
        (
            saving,
            "'inflow'",
            "NULL",
            "NULL",
            "a goal that brings money in",
        ),
        (
            saving,
            "'outflow'",
            "'budget'",
            "'saving_toward'",
            "a goal with a spending kind",
        ),
        (
            saving,
            "'outflow'",
            "NULL",
            "'flexible'",
            "a goal that is not saving toward",
        ),
    ];
    for (item, direction, spending, tier, what) in refused {
        let direction = direction.trim_matches('\'');
        assert!(
            write_version(&pool, &world, item, 2, direction, spending, tier)
                .await
                .is_err(),
            "{what}"
        );
    }
    for (seq, (item, direction, spending, tier, what)) in (3..).zip([
        (bill, "outflow", "'budget'", "'flexible'", "a budget"),
        (bill, "inflow", "NULL", "NULL", "income"),
        (one_time, "outflow", "NULL", "'essentials'", "a one-off"),
        (saving, "outflow", "NULL", "'saving_toward'", "a goal"),
    ]) {
        write_version(&pool, &world, item, seq, direction, spending, tier)
            .await
            .unwrap_or_else(|error| panic!("{what}: {error}"));
    }
    pool.close().await;
}

#[tokio::test]
async fn a_version_is_never_updated_or_deleted_alone() {
    let world = world().await;
    let pool = world.pool().await;
    let bill = world.insert(world.new_item(monthly())).await;

    let update = sqlx::query("UPDATE planned_item_versions SET amount_minor = 1")
        .execute(&pool)
        .await;
    let delete = sqlx::query("DELETE FROM planned_item_versions")
        .execute(&pool)
        .await;

    assert!(update.is_err(), "an update is refused");
    assert!(delete.is_err(), "a delete is refused");
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM planned_item_versions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
    pool.close().await;

    let mut tx = world.store.begin().await.unwrap();
    tx.delete_planned_item(bill).await.unwrap();
    tx.commit().await.unwrap();
    let pool = world.pool().await;
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM planned_item_versions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the item's own delete takes its versions with it");
    pool.close().await;
}

#[tokio::test]
async fn the_store_refuses_to_insert_values_that_do_not_fit() {
    let world = world().await;
    let mut tx = world.store.begin().await.unwrap();
    let mut spending_on_a_goal = world.new_item(goal());
    spending_on_a_goal.spending = Some(PlanSpending::Bill);
    let mut goal_tier_on_a_bill = world.new_item(monthly());
    goal_tier_on_a_bill.tier = Some(PlanTier::SavingToward);
    let mut goal_in = world.new_item(goal());
    goal_in.direction = PlanDirection::Inflow;
    goal_in.tier = None;

    for new in [spending_on_a_goal, goal_tier_on_a_bill, goal_in] {
        assert!(matches!(
            tx.insert_planned_item(new).await,
            Err(StoreError::Invalid(_))
        ));
    }
}
