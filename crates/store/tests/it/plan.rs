//! Planned items in the store: every schedule survives a round trip, the
//! schema refuses a row whose columns don't fit its type, and a value that no
//! longer parses is a backend error, never a panic.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use coffer_domain::accounts::{AccountKind, AccountName, LiquidityClass};
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{
    IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, PlannedItemName, spending_applies,
};
use coffer_domain::transactions::{
    CategoryGroupName, CategoryKind, CategoryName, Description, Notes, SplitInput,
    TransactionOrigin,
};
use coffer_store::{
    Attribution, NewAccount, NewCategory, NewCategoryGroup, NewPlannedItem, NewTransaction,
    PlannedItemChanges, Store, StoreError,
};
use jiff::Timestamp;
use jiff::civil::date;
use tempfile::TempDir;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("USD").unwrap(),
    }
}

struct World {
    dir: TempDir,
    store: Store,
    category: CategoryId,
    account: AccountId,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let group = tx
        .insert_category_group(NewCategoryGroup {
            name: CategoryGroupName::parse("Home").unwrap(),
            position: None,
            created_at: at(0),
        })
        .await
        .unwrap();
    let category = tx
        .insert_category(NewCategory {
            group: group.id,
            name: CategoryName::parse("Utilities").unwrap(),
            kind: CategoryKind::Expense,
            position: None,
            created_at: at(0),
        })
        .await
        .unwrap();
    let account = tx
        .insert_account(NewAccount {
            id: AccountId::new(),
            name: AccountName::parse("Checking").unwrap(),
            kind: AccountKind::Checking,
            currency: Currency::parse("USD").unwrap(),
            liquidity: LiquidityClass::Cash,
            liquidity_overridden: false,
            include_in_net_worth: true,
            include_in_forecast: true,
            created_at: at(0),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    World {
        dir,
        store,
        category: category.id,
        account: account.id,
    }
}

impl World {
    fn new_item(&self, schedule: PlanSchedule) -> NewPlannedItem {
        NewPlannedItem {
            name: PlannedItemName::parse("Water bill").unwrap(),
            category: self.category,
            amount: usd(4_200),
            direction: PlanDirection::Outflow,
            account_hint: None,
            notes: None,
            active: true,
            schedule,
            spending: spending_applies(PlanDirection::Outflow, &schedule)
                .then_some(PlanSpending::Bill),
            created_at: at(1),
        }
    }

    async fn insert(&self, new: NewPlannedItem) -> PlannedItemId {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx.insert_planned_item(new).await.unwrap();
        tx.commit().await.unwrap();
        record.id
    }

    /// A direct connection, since no store call can write a row the types
    /// refuse.
    async fn pool(&self) -> sqlx::SqlitePool {
        sqlx::SqlitePool::connect(&format!(
            "sqlite://{}",
            self.dir.path().join("coffer.db").display()
        ))
        .await
        .unwrap()
    }
}

fn one_off() -> PlanSchedule {
    PlanSchedule::OneOff {
        on: date(2027, 3, 15),
    }
}

fn schedules() -> [PlanSchedule; 4] {
    [
        PlanSchedule::Recurring {
            every: 2,
            unit: IntervalUnit::Months,
            anchor_on: date(2027, 1, 31),
            end_on: Some(date(2028, 1, 31)),
        },
        PlanSchedule::Recurring {
            every: 999,
            unit: IntervalUnit::Days,
            anchor_on: date(2027, 1, 1),
            end_on: None,
        },
        one_off(),
        PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2027, 12, 1),
            completed: true,
        },
    ]
}

#[tokio::test]
async fn planned_item_roundtrips_each_schedule() {
    let world = world().await;
    for schedule in schedules() {
        let mut new = world.new_item(schedule);
        new.account_hint = Some(world.account);
        new.notes = Some(Notes::parse("first line\nsecond line").unwrap());
        new.active = false;
        let expected_notes = new.notes.clone();
        let expected_spending = new.spending;

        let id = world.insert(new).await;

        let record = world.store.get_planned_item(id).await.unwrap();
        assert_eq!(record.schedule, schedule, "the schedule comes back whole");
        assert_eq!(record.name.as_str(), "Water bill");
        assert_eq!(record.category, world.category);
        assert_eq!(record.amount, usd(4_200));
        assert_eq!(record.direction, PlanDirection::Outflow);
        assert_eq!(record.account_hint, Some(world.account));
        assert_eq!(record.notes, expected_notes);
        assert!(!record.active, "the active flag comes back");
        assert_eq!(record.spending, expected_spending);
        assert_eq!(record.created_at, at(1));
        assert_eq!(record.updated_at, at(1));

        let mut tx = world.store.begin().await.unwrap();
        assert_eq!(
            tx.get_planned_item(id).await.unwrap(),
            record,
            "a transaction reads the same record"
        );
    }
}

#[tokio::test]
async fn update_changes_only_the_named_columns() {
    let world = world().await;
    let mut new = world.new_item(one_off());
    new.account_hint = Some(world.account);
    new.notes = Some(Notes::parse("keep me").unwrap());
    let id = world.insert(new).await;

    let mut tx = world.store.begin().await.unwrap();
    let record = tx
        .update_planned_item(
            id,
            PlannedItemChanges {
                amount: Some(usd(1)),
                account_hint: Some(None),
                schedule: Some(PlanSchedule::OneOff {
                    on: date(2027, 4, 1),
                }),
                ..PlannedItemChanges::default()
            },
            at(9),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(record.amount, usd(1));
    assert_eq!(record.account_hint, None, "Some(None) clears the hint");
    assert_eq!(
        record.schedule,
        PlanSchedule::OneOff {
            on: date(2027, 4, 1)
        }
    );
    assert_eq!(record.notes.unwrap().as_str(), "keep me", "None keeps it");
    assert_eq!(record.name.as_str(), "Water bill");
    assert_eq!(record.created_at, at(1));
    assert_eq!(record.updated_at, at(9));
}

#[tokio::test]
async fn spending_roundtrips_and_changes() {
    let world = world().await;
    let schedule = schedules()[0];
    let mut new = world.new_item(schedule);
    new.spending = Some(PlanSpending::Budget);
    let id = world.insert(new).await;
    let read = world.store.get_planned_item(id).await.unwrap();
    assert_eq!(read.spending, Some(PlanSpending::Budget));

    let mut tx = world.store.begin().await.unwrap();
    let record = tx
        .update_planned_item(
            id,
            PlannedItemChanges {
                spending: Some(Some(PlanSpending::Bill)),
                ..PlannedItemChanges::default()
            },
            at(3),
        )
        .await
        .unwrap();
    assert_eq!(record.spending, Some(PlanSpending::Bill));
    // An inflow has none, so the two change together.
    let record = tx
        .update_planned_item(
            id,
            PlannedItemChanges {
                direction: Some(PlanDirection::Inflow),
                spending: Some(None),
                ..PlannedItemChanges::default()
            },
            at(4),
        )
        .await
        .unwrap();
    assert_eq!(record.spending, None);
    let refused = tx
        .update_planned_item(
            id,
            PlannedItemChanges {
                spending: Some(Some(PlanSpending::Bill)),
                ..PlannedItemChanges::default()
            },
            at(5),
        )
        .await;
    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "the table's check refuses a spending kind on an inflow"
    );
}

#[tokio::test]
async fn store_never_changes_a_schedules_type() {
    let world = world().await;
    let id = world.insert(world.new_item(one_off())).await;

    let mut tx = world.store.begin().await.unwrap();
    let refused = tx
        .update_planned_item(
            id,
            PlannedItemChanges {
                schedule: Some(PlanSchedule::Goal {
                    start_on: date(2027, 1, 1),
                    due_on: date(2027, 2, 1),
                    completed: false,
                }),
                ..PlannedItemChanges::default()
            },
            at(2),
        )
        .await;

    assert!(
        matches!(refused, Err(StoreError::Backend(_))),
        "the table's check refuses another type's columns"
    );
}

#[tokio::test]
async fn delete_and_missing_items_answer_not_found() {
    let world = world().await;
    let id = world.insert(world.new_item(one_off())).await;

    let mut tx = world.store.begin().await.unwrap();
    assert_eq!(tx.count_planned_items().await.unwrap(), 1);
    tx.delete_planned_item(id).await.unwrap();
    assert_eq!(tx.count_planned_items().await.unwrap(), 0);
    assert!(matches!(
        tx.delete_planned_item(id).await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        tx.update_planned_item(id, PlannedItemChanges::default(), at(2))
            .await,
        Err(StoreError::NotFound)
    ));
    tx.commit().await.unwrap();

    assert!(matches!(
        world.store.get_planned_item(id).await,
        Err(StoreError::NotFound)
    ));
}

/// The schema by itself holds a linked goal, whatever the ledger checks first.
#[tokio::test]
async fn delete_refuses_a_goal_with_a_linked_split() {
    let world = world().await;
    let goal = world
        .insert(world.new_item(PlanSchedule::Goal {
            start_on: date(2027, 1, 1),
            due_on: date(2027, 7, 1),
            completed: false,
        }))
        .await;
    let mut tx = world.store.begin().await.unwrap();
    let transaction = tx
        .insert_transaction(NewTransaction {
            origin: TransactionOrigin::Manual,
            account: world.account,
            posted_on: date(2027, 1, 10),
            transacted_on: None,
            pending: false,
            amount_minor: -500,
            currency: Currency::parse("USD").unwrap(),
            raw_description: Description::parse("Purchase").unwrap(),
            imported_payee: None,
            memo: None,
            notes: None,
            created_at: at(1),
        })
        .await
        .unwrap();
    tx.replace_splits(
        transaction.id,
        &[SplitInput {
            category: world.category,
            minor_units: -500,
            goal: Some(goal),
        }],
        Attribution {
            member: None,
            agent: None,
            system: Some("system:test".into()),
            at: at(2),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = world.store.begin().await.unwrap();
    let deleted = tx.delete_planned_item(goal).await;

    assert!(
        matches!(deleted, Err(StoreError::Backend(_))),
        "the reference refuses the delete"
    );
}

#[tokio::test]
async fn list_is_ordered_by_id_and_keeps_inactive_only_when_asked() {
    let world = world().await;
    let active = world.insert(world.new_item(one_off())).await;
    let mut inactive = world.new_item(one_off());
    inactive.active = false;
    let inactive = world.insert(inactive).await;
    let later = world.insert(world.new_item(one_off())).await;

    let ids = |records: Vec<coffer_store::PlannedItemRecord>| {
        records.iter().map(|record| record.id).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(world.store.list_planned_items(false).await.unwrap()),
        [active, later]
    );
    assert_eq!(
        ids(world.store.list_planned_items(true).await.unwrap()),
        [active, inactive, later]
    );
}

/// Rows the schema refuses: direction, item type, then the schedule's columns
/// and the spending kind. Each sets columns that don't fit its type.
const REFUSED_ROWS: &[(&str, &str, &str)] = &[
    // A recurring item without its anchor.
    (
        "outflow",
        "recurring",
        "1, 'months', NULL, NULL, NULL, NULL, NULL, NULL, 'bill'",
    ),
    // A recurring item that also has a one-off date.
    (
        "outflow",
        "recurring",
        "1, 'months', '2027-01-01', NULL, '2027-01-01', NULL, NULL, NULL, 'bill'",
    ),
    // A recurring item that also has a goal's columns.
    (
        "outflow",
        "recurring",
        "1, 'months', '2027-01-01', NULL, NULL, '2027-01-01', '2027-02-01', 0, 'bill'",
    ),
    // A recurring outflow without a spending kind.
    (
        "outflow",
        "recurring",
        "1, 'months', '2027-01-01', NULL, NULL, NULL, NULL, NULL, NULL",
    ),
    // A recurring outflow with an unknown one.
    (
        "outflow",
        "recurring",
        "1, 'months', '2027-01-01', NULL, NULL, NULL, NULL, NULL, 'splurge'",
    ),
    // A recurring inflow with one.
    (
        "inflow",
        "recurring",
        "1, 'months', '2027-01-01', NULL, NULL, NULL, NULL, NULL, 'bill'",
    ),
    // A one-off without its date.
    (
        "outflow",
        "one_off",
        "NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL",
    ),
    // A one-off with a recurring item's end date.
    (
        "outflow",
        "one_off",
        "NULL, NULL, NULL, '2027-01-01', '2027-01-01', NULL, NULL, NULL, NULL",
    ),
    // A one-off with a spending kind.
    (
        "outflow",
        "one_off",
        "NULL, NULL, NULL, NULL, '2027-01-01', NULL, NULL, NULL, 'bill'",
    ),
    // A goal without its completed flag.
    (
        "outflow",
        "goal",
        "NULL, NULL, NULL, NULL, NULL, '2027-01-01', '2027-02-01', NULL, NULL",
    ),
    // A goal that brings money in.
    (
        "inflow",
        "goal",
        "NULL, NULL, NULL, NULL, NULL, '2027-01-01', '2027-02-01', 0, NULL",
    ),
    // A goal with a spending kind.
    (
        "outflow",
        "goal",
        "NULL, NULL, NULL, NULL, NULL, '2027-01-01', '2027-02-01', 0, 'budget'",
    ),
    // A date that isn't one.
    (
        "outflow",
        "one_off",
        "NULL, NULL, NULL, NULL, 'soon', NULL, NULL, NULL, NULL",
    ),
];

#[tokio::test]
async fn check_refuses_mixed_schedule_columns() {
    let world = world().await;
    let pool = world.pool().await;
    let insert = |direction: &str, item_type: &str, schedule: &str| {
        format!(
            "INSERT INTO planned_items (id, name, category_id, amount_minor, currency, direction, \
             active, item_type, every, unit, anchor_on, end_on, one_off_on, goal_start_on, \
             goal_due_on, goal_completed, spending, created_at, updated_at) VALUES ('{id}', 'Water bill', \
             '{category}', 100, 'USD', '{direction}', 1, '{item_type}', {schedule}, 0, 0)",
            id = PlannedItemId::new(),
            category = world.category,
        )
    };
    for (direction, item_type, schedule) in REFUSED_ROWS {
        let sql = insert(direction, item_type, schedule);
        assert!(
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&pool)
                .await
                .is_err(),
            "the schema refuses a row whose columns don't fit its type"
        );
    }
    for (direction, item_type, schedule) in [
        (
            "outflow",
            "goal",
            "NULL, NULL, NULL, NULL, NULL, '2027-01-01', '2027-02-01', 0, NULL",
        ),
        (
            "outflow",
            "recurring",
            "1, 'months', '2027-01-01', NULL, NULL, NULL, NULL, NULL, 'budget'",
        ),
        (
            "inflow",
            "recurring",
            "1, 'months', '2027-01-01', NULL, NULL, NULL, NULL, NULL, NULL",
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(insert(direction, item_type, schedule)))
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;
}

#[tokio::test]
async fn corrupt_currency_is_a_backend_error() {
    let world = world().await;
    let id = world.insert(world.new_item(one_off())).await;

    world
        .store
        .execute_unchecked_for_tests("UPDATE planned_items SET currency = ?", "ZZZ")
        .await
        .unwrap();

    assert!(matches!(
        world.store.get_planned_item(id).await,
        Err(StoreError::Backend(_))
    ));
    assert!(matches!(
        world.store.list_planned_items(true).await,
        Err(StoreError::Backend(_))
    ));
}

#[tokio::test]
async fn corrupt_spending_is_a_backend_error() {
    let world = world().await;
    let recurring = world.insert(world.new_item(schedules()[0])).await;
    world
        .store
        .execute_unchecked_for_tests(
            "UPDATE planned_items SET spending = ? WHERE item_type = 'recurring'",
            "splurge",
        )
        .await
        .unwrap();
    assert!(
        matches!(
            world.store.get_planned_item(recurring).await,
            Err(StoreError::Backend(_))
        ),
        "an unknown kind is corruption"
    );

    let other = world.insert(world.new_item(one_off())).await;
    world
        .store
        .execute_unchecked_for_tests(
            "UPDATE planned_items SET spending = ? WHERE item_type = 'one_off'",
            "bill",
        )
        .await
        .unwrap();
    assert!(
        matches!(
            world.store.get_planned_item(other).await,
            Err(StoreError::Backend(_))
        ),
        "a kind on an item that can't have one is corruption"
    );
}

#[tokio::test]
async fn corrupt_schedule_is_a_backend_error() {
    let world = world().await;
    let id = world.insert(world.new_item(one_off())).await;

    world
        .store
        .execute_unchecked_for_tests("UPDATE planned_items SET one_off_on = ?", "2027-02-30")
        .await
        .unwrap();
    assert!(matches!(
        world.store.get_planned_item(id).await,
        Err(StoreError::Backend(_))
    ));

    world
        .store
        .execute_unchecked_for_tests("UPDATE planned_items SET one_off_on = ?", "1960-01-01")
        .await
        .unwrap();
    assert!(
        matches!(
            world.store.get_planned_item(id).await,
            Err(StoreError::Backend(_))
        ),
        "a date the domain would refuse is corruption too"
    );
}
