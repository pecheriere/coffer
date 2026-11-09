//! What the tables of migration 0022 accept and refuse once it has run: each kind, each
//! `belongs_to`, the beneficiary and its limit.

use super::*;

#[tokio::test]
async fn every_kind_passes_the_tables() {
    let (_dir, pool) = migrated_empty().await;
    run(
        &pool,
        "INSERT INTO members (id, username, display_name, role, created_at) \
         VALUES ('member-a', 'member-a', '', 'admin', 0)",
    )
    .await
    .unwrap();
    run(
        &pool,
        "INSERT INTO connections (id, provider, label, owner_id, status, created_at) \
         VALUES ('bank', 'simplefin', 'Bank', 'member-a', 'active', 0)",
    )
    .await
    .unwrap();
    for kind in AccountKind::ALL {
        let kind = kind.as_str();
        run(
            &pool,
            &holding(&format!(
                "'Name', '{kind}', 'USD', 'cash', 0, 1, 1, 'household', NULL"
            )),
        )
        .await
        .expect(kind);
        run(&pool, &feed(&format!("'{kind}', '{kind}', NULL, NULL")))
            .await
            .expect(kind);
    }
}

#[tokio::test]
async fn every_value_of_belongs_to_passes() {
    let (_dir, pool) = migrated_empty().await;
    for belongs in [
        BelongsTo::Household,
        BelongsTo::SomeoneElse,
        BelongsTo::GivenAway,
    ] {
        let belongs = belongs.as_str();
        run(
            &pool,
            &holding(&format!(
                "'Name', 'checking', 'USD', 'cash', 0, 0, 0, '{belongs}', NULL"
            )),
        )
        .await
        .expect(belongs);
    }
}

#[tokio::test]
async fn an_unknown_belongs_to_is_refused() {
    let (_dir, pool) = migrated_empty().await;
    assert_refused_by_check(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 0, 0, 'nobody', NULL"),
    )
    .await;
}

#[tokio::test]
async fn not_the_household_can_t_be_counted() {
    let (_dir, pool) = migrated_empty().await;
    for values in [
        "'Name', 'education', 'USD', 'invested', 0, 1, 0, 'someone_else', NULL",
        "'Name', 'education', 'USD', 'invested', 0, 0, 1, 'someone_else', NULL",
        "'Name', 'charitable', 'USD', 'invested', 0, 1, 0, 'given_away', NULL",
        "'Name', 'charitable', 'USD', 'invested', 0, 0, 1, 'given_away', NULL",
    ] {
        assert_refused_by_check(&pool, &holding(values)).await;
    }
    run(
        &pool,
        &holding("'Name', 'education', 'USD', 'invested', 0, 0, 0, 'someone_else', NULL"),
    )
    .await
    .unwrap();
    for flag in ["include_in_net_worth", "include_in_forecast"] {
        assert_refused_by_check(&pool, &format!("UPDATE accounts SET {flag} = 1")).await;
    }
    run(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL"),
    )
    .await
    .unwrap();
    assert_refused_by_check(
        &pool,
        "UPDATE accounts SET belongs_to = 'given_away' WHERE belongs_to = 'household'",
    )
    .await;
}

#[tokio::test]
async fn the_household_has_no_beneficiary() {
    let (_dir, pool) = migrated_empty().await;
    assert_refused_by_check(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', 'Child A'"),
    )
    .await;
    run(
        &pool,
        &holding("'Name', 'checking', 'USD', 'cash', 0, 1, 1, 'household', NULL"),
    )
    .await
    .unwrap();
    assert_refused_by_check(&pool, "UPDATE accounts SET beneficiary = 'Child A'").await;
}

#[tokio::test]
async fn belongs_to_overridden_is_a_flag() {
    let (_dir, pool) = migrated_empty().await;
    let with = |flag: i64| {
        format!(
            "INSERT INTO accounts (id, name, kind, currency, liquidity, liquidity_overridden, \
             include_in_net_worth, include_in_forecast, belongs_to_overridden, created_at) \
             VALUES (lower(hex(randomblob(18))), 'Name', 'checking', 'USD', 'cash', 0, 1, 1, \
             {flag}, 0)"
        )
    };
    run(&pool, &with(1)).await.unwrap();
    assert_refused_by_check(&pool, &with(2)).await;
}

#[tokio::test]
async fn a_stored_beneficiary_respects_its_maximum() {
    let (_dir, pool) = migrated_empty().await;
    let with = |label: &str| {
        holding(&format!(
            "'Name', 'custodial', 'USD', 'invested', 0, 0, 0, 'someone_else', {label}"
        ))
    };
    run(&pool, &with("printf('%64s', 'x')")).await.unwrap();
    assert_refused_by_check(&pool, &with("printf('%65s', 'x')")).await;
    assert_refused_by_check(&pool, &with("''")).await;
}

fn college_fund() -> NewAccount {
    NewAccount {
        id: AccountId::new(),
        name: AccountName::parse("College fund").unwrap(),
        kind: AccountKind::Education,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Invested,
        liquidity_overridden: false,
        include_in_net_worth: false,
        include_in_forecast: false,
        created_at: at(),
        belongs_to: BelongsTo::SomeoneElse,
        belongs_to_overridden: true,
        beneficiary: Some(Beneficiary::parse("Child A").unwrap()),
    }
}

/// A record's or a change's `Debug` can reach a log line; a beneficiary may be a child's name.
#[tokio::test]
async fn debug_output_holds_no_beneficiary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let new = college_fund();
    let mut tx = store.begin().await.unwrap();
    let inserted = tx.insert_account(new.clone()).await.unwrap();
    let changes = AccountChanges {
        beneficiary: Some(Some(Beneficiary::parse("Child A").unwrap())),
        ..AccountChanges::default()
    };
    for shown in [
        format!("{inserted:?}"),
        format!("{new:?}"),
        format!("{changes:?}"),
    ] {
        assert!(!shown.contains("Child A"), "the label stays out");
        assert!(shown.contains("<redacted>"), "and says it was held back");
    }
}

#[tokio::test]
async fn belonging_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let new = college_fund();
    let id = new.id;
    let mut tx = store.begin().await.unwrap();
    let inserted = tx.insert_account(new).await.unwrap();
    assert_eq!(
        tx.get_account(id).await.unwrap(),
        inserted,
        "reads as written"
    );
    assert_eq!(inserted.belongs_to, BelongsTo::SomeoneElse);
    assert!(inserted.belongs_to_overridden);
    assert_eq!(
        inserted.beneficiary.as_ref().map(Beneficiary::as_str),
        Some("Child A")
    );

    let untouched = tx
        .update_account(id, AccountChanges::default())
        .await
        .unwrap();
    assert_eq!(untouched, inserted, "no change leaves all three");

    let changed = tx
        .update_account(
            id,
            AccountChanges {
                belongs_to: Some(BelongsTo::GivenAway),
                belongs_to_overridden: Some(false),
                beneficiary: Some(Some(Beneficiary::parse("Family fund").unwrap())),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(tx.get_account(id).await.unwrap(), changed);
    assert_eq!(changed.belongs_to, BelongsTo::GivenAway);
    assert!(!changed.belongs_to_overridden);
    assert_eq!(
        changed.beneficiary.as_ref().map(Beneficiary::as_str),
        Some("Family fund")
    );

    let cleared = tx
        .update_account(
            id,
            AccountChanges {
                beneficiary: Some(None),
                ..AccountChanges::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.beneficiary, None, "Some(None) clears");
    assert_eq!(cleared.belongs_to, BelongsTo::GivenAway, "and only that");
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn changing_belonging_writes_no_daily_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    let store = Store::open(&path).await.unwrap();
    let mut new = college_fund();
    new.belongs_to = BelongsTo::Household;
    new.belongs_to_overridden = false;
    new.beneficiary = None;
    let id = new.id;
    let mut tx = store.begin().await.unwrap();
    tx.insert_account(new).await.unwrap();
    tx.upsert_balance(BalanceSnapshot {
        account: id,
        on: date(2026, 10, 1),
        balance_minor: 100,
        source: BalanceSource::Manual,
        recorded_at: at(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    store.rebuild_net_worth(date(2026, 10, 3)).await.unwrap();
    let pool = connect(&path).await;
    let columns = columns_of(&pool, "account_daily_values").await;
    let before = dump(&pool, "account_daily_values", &columns).await;
    assert_eq!(before.len(), 3, "the table holds the three days");

    let mut tx = store.begin().await.unwrap();
    tx.update_account(
        id,
        AccountChanges {
            belongs_to: Some(BelongsTo::SomeoneElse),
            belongs_to_overridden: Some(true),
            beneficiary: Some(Some(Beneficiary::parse("Child A").unwrap())),
            ..AccountChanges::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        dump(&pool, "account_daily_values", &columns).await,
        before,
        "belonging applies at read"
    );
}

#[tokio::test]
async fn the_store_reads_a_migrated_row() {
    let (dir, pool) = seeded().await;
    every_migration().run(&pool).await.unwrap();
    pool.close().await;
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let read = tx.get_account(FIRST.parse().unwrap()).await.unwrap();
    assert_eq!(read.belongs_to, BelongsTo::Household);
    assert!(!read.belongs_to_overridden);
    assert_eq!(read.beneficiary, None);
}
