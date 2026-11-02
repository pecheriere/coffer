#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]
use coffer_domain::audit::{AuditActor, AuditChanges, AuditEntry};
use coffer_domain::ids::{AgentTokenId, MemberId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{ForecastSettings, SafetyFloor};
use coffer_store::{AuditFilter, PageRequest, Store, StoreError};
use jiff::Timestamp;
use tempfile::TempDir;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap()
}

fn entry(action: &str, actor: AuditActor) -> AuditEntry {
    AuditEntry {
        actor,
        action: action.into(),
        entity_type: "member".into(),
        entity_id: "m1".into(),
        changes: AuditChanges::new().set("k", action),
    }
}

fn system() -> AuditActor {
    AuditActor::System {
        reason: "test".into(),
    }
}

async fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("coffer.db")).await.unwrap();
    (dir, store)
}

fn page(limit: u32, after: Option<String>) -> PageRequest {
    PageRequest { after, limit }
}

#[tokio::test]
async fn open_twice_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coffer.db");
    drop(Store::open(&path).await.unwrap());
    Store::open(&path).await.unwrap();
}

#[tokio::test]
async fn uncommitted_tx_leaves_no_audit() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.append_audit(at(1), &entry("a", system())).await.unwrap();
    drop(tx);

    let listed = store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 0);
    assert_eq!(listed.next, None);
}

#[tokio::test]
async fn committed_audit_is_listed_newest_first_with_cursor() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    for (i, action) in ["one", "two", "three"].into_iter().enumerate() {
        tx.append_audit(
            at(1_000 + i64::try_from(i).unwrap()),
            &entry(action, system()),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let first = store
        .list_audit(&AuditFilter::default(), &page(2, None))
        .await
        .unwrap();
    let actions: Vec<_> = first.items.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["three", "two"]);
    assert_eq!(first.items[0].at, at(1_002));
    assert_eq!(
        first.items[0].changes,
        AuditChanges::new().set("k", "three")
    );
    assert_eq!(first.items[0].actor, system());
    let cursor = first.next.expect("a second page exists");

    let second = store
        .list_audit(&AuditFilter::default(), &page(2, Some(cursor)))
        .await
        .unwrap();
    let actions: Vec<_> = second.items.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["one"]);
    assert_eq!(second.next, None);
}

#[tokio::test]
async fn audit_filter_by_agent() {
    let (_dir, store) = open().await;
    let member = MemberId::new();
    let agent = AgentTokenId::new();
    let mut tx = store.begin().await.unwrap();
    tx.append_audit(
        at(1),
        &entry(
            "via-agent",
            AuditActor::Member {
                member,
                agent: Some(agent),
            },
        ),
    )
    .await
    .unwrap();
    tx.append_audit(
        at(2),
        &entry(
            "via-session",
            AuditActor::Member {
                member,
                agent: None,
            },
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let filter = AuditFilter {
        agent: Some(agent),
        ..AuditFilter::default()
    };
    let listed = store
        .list_audit(&filter, &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].action, "via-agent");
    assert_eq!(
        listed.items[0].actor,
        AuditActor::Member {
            member,
            agent: Some(agent)
        }
    );
}

#[tokio::test]
async fn audit_filter_by_entity_member_and_time_range() {
    let (_dir, store) = open().await;
    let member = MemberId::new();
    let mut tx = store.begin().await.unwrap();
    for ms in [10, 20, 30] {
        tx.append_audit(
            at(ms),
            &entry(
                &format!("t{ms}"),
                AuditActor::Member {
                    member,
                    agent: None,
                },
            ),
        )
        .await
        .unwrap();
    }
    tx.append_audit(at(20), &entry("other", system()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let filter = AuditFilter {
        entity_type: Some("member".into()),
        entity_id: Some("m1".into()),
        member: Some(member),
        from: Some(at(20)),
        to: Some(at(30)),
        ..AuditFilter::default()
    };
    let listed = store
        .list_audit(&filter, &PageRequest::default())
        .await
        .unwrap();
    let actions: Vec<_> = listed.items.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["t20"]);
}

#[tokio::test]
async fn invalid_cursor_is_rejected() {
    let (_dir, store) = open().await;
    let err = store
        .list_audit(
            &AuditFilter::default(),
            &page(10, Some("not-a-cursor".into())),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::InvalidCursor));
}

#[tokio::test]
async fn page_limit_is_clamped() {
    let (_dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.append_audit(at(1), &entry("a", system())).await.unwrap();
    tx.append_audit(at(2), &entry("b", system())).await.unwrap();
    tx.commit().await.unwrap();

    let listed = store
        .list_audit(&AuditFilter::default(), &page(0, None))
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert!(listed.next.is_some());
}

#[tokio::test]
async fn backup_produces_openable_copy() {
    let (dir, store) = open().await;
    let mut tx = store.begin().await.unwrap();
    tx.append_audit(at(1), &entry("a", system())).await.unwrap();
    tx.commit().await.unwrap();

    let backup = dir.path().join("backup.db");
    store.backup_to(&backup).await.unwrap();

    let copy = Store::open(&backup).await.unwrap();
    let listed = copy
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].action, "a");

    assert!(matches!(
        store.backup_to(&backup).await,
        Err(StoreError::Conflict(_))
    ));
}

fn usd(minor_units: i64) -> SafetyFloor {
    SafetyFloor::Fixed {
        amount: Money {
            minor_units,
            currency: Currency::parse("USD").unwrap(),
        },
    }
}

async fn save(store: &Store, settings: &ForecastSettings) {
    let mut tx = store.begin().await.unwrap();
    tx.set_forecast_settings(settings).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn settings_roundtrip_each_floor_kind() {
    let (_dir, store) = open().await;
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        ForecastSettings::default(),
        "no row reads as the defaults"
    );

    let fixed_zero = ForecastSettings {
        floor: Some(usd(0)),
        include_income: true,
        default_horizon_months: 12,
        stale_after_days: 30,
    };
    save(&store, &fixed_zero).await;
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        fixed_zero,
        "a fixed floor of 0 is stored as 0, not unset"
    );

    let months = ForecastSettings {
        floor: Some(SafetyFloor::MonthsOfOutflows { months: 6 }),
        ..fixed_zero
    };
    save(&store, &months).await;
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        months,
        "a months floor replaces a fixed one"
    );

    let mut tx = store.begin().await.unwrap();
    assert_eq!(
        tx.forecast_settings().await.unwrap(),
        months,
        "a transaction reads the same"
    );
    tx.set_forecast_settings(&ForecastSettings::default())
        .await
        .unwrap();
    assert_eq!(
        tx.forecast_settings().await.unwrap(),
        ForecastSettings::default(),
        "a transaction sees its own write, floor cleared"
    );
    drop(tx);
    assert_eq!(
        store.forecast_settings().await.unwrap(),
        months,
        "a dropped transaction stores nothing"
    );
}

/// A floor's kind, minor units, currency and months, as columns.
type FloorColumns = (
    Option<&'static str>,
    Option<i64>,
    Option<&'static str>,
    Option<i64>,
);

#[tokio::test]
async fn check_constraints_refuse_a_half_floor() {
    let (dir, store) = open().await;
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        dir.path().join("coffer.db").display()
    ))
    .await
    .unwrap();
    let insert = "INSERT INTO household_settings (id, floor_kind, floor_minor, floor_currency, \
                  floor_months, include_income, default_horizon_months, stale_after_days) \
                  VALUES (1, ?, ?, ?, ?, 0, 3, 7)";
    let cases: [FloorColumns; 9] = [
        (Some("fixed"), None, None, None),
        (Some("fixed"), Some(1), None, None),
        (Some("fixed"), None, Some("USD"), None),
        (Some("fixed"), Some(1), Some("USD"), Some(3)),
        (Some("months_of_outflows"), None, None, None),
        (Some("months_of_outflows"), Some(1), None, Some(3)),
        (None, Some(1), Some("USD"), None),
        (None, None, None, Some(3)),
        (Some("percent"), None, None, Some(3)),
    ];
    for (kind, minor, currency, months) in cases {
        let result = sqlx::query(insert)
            .bind(kind)
            .bind(minor)
            .bind(currency)
            .bind(months)
            .execute(&pool)
            .await;
        assert!(result.is_err(), "a half or mixed floor is refused");
    }
    let whole = sqlx::query(insert)
        .bind(Some("fixed"))
        .bind(Some(0_i64))
        .bind(Some("USD"))
        .bind(None::<i64>)
        .execute(&pool)
        .await;
    assert!(whole.is_ok(), "a whole fixed floor is accepted");
    pool.close().await;
    assert_eq!(
        store.forecast_settings().await.unwrap().floor,
        Some(usd(0)),
        "the stored row reads back"
    );
}
