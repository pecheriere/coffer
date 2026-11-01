//! Binding provider accounts to ledger accounts, and creating accounts from
//! them.

use super::{PLAINTEXT, denied, reported, txn, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::AccountKind;
use coffer_domain::audit::AuditActor;
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ProviderAccountId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::{AccountAccess, AccountLevel, AccountScope, Action, DenyReason};
use coffer_domain::time::Clock as _;
use coffer_server::ledger::accounts::NewAccountInput;
use coffer_server::sync::SyncError;
use coffer_server::sync::provider::ProviderKind;
use coffer_store::{AuditEvent, ProviderAccountUpsert};
use jiff::civil::date;
use serde_json::json;

#[tokio::test]
async fn bind_requires_same_currency() {
    let world = world().await;
    world
        .sync_once(vec![
            reported("acct-usd", "USD", None, vec![txn("t-1", "-4.50")]),
            reported("acct-odd", "DOGE", None, Vec::new()),
        ])
        .await;
    let usd = world.provider_account("acct-usd").await;
    let odd = world.provider_account("acct-odd").await;
    let euros = world.account("Euro account", "EUR").await;

    let err = world
        .server
        .sync
        .bind(&world.alice, usd.id, euros)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::CurrencyMismatch));
    let err = world
        .server
        .sync
        .bind(&world.alice, odd.id, euros)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::CurrencyMismatch));

    assert_eq!(world.provider_account("acct-usd").await.account, None);
    assert_eq!(world.transactions(euros).await.len(), 0);
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 0);
}

#[tokio::test]
async fn bind_applies_held_records() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            None,
            vec![txn("t-1", "-4.50"), txn("t-2", "1200.00")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    assert_eq!(
        world.transactions(checking).await.len(),
        0,
        "records of an unbound provider account are held"
    );

    let reader = world.token(Scope::Read).await;
    let (status, unbound) = world
        .call(
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &reader,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unbound["items"][0]["id"], json!(held.id.to_string()));
    assert_eq!(unbound["items"][0]["currency"], json!("USD"));

    let bound = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();
    assert_eq!(bound.account, Some(checking));

    let mut amounts: Vec<i64> = world
        .transactions(checking)
        .await
        .iter()
        .map(|transaction| transaction.amount_minor)
        .collect();
    amounts.sort_unstable();
    assert_eq!(amounts, [-450, 120_000]);

    let binds = world.audit_actions("provider_account.bind").await;
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].entity_id, held.id.to_string());
    assert_eq!(
        binds[0].actor,
        AuditActor::Member {
            member: world.alice.member,
            agent: None
        }
    );

    let (_, unbound) = world
        .call(
            Method::GET,
            "/api/v1/provider-accounts?unbound=true",
            &reader,
            None,
        )
        .await;
    assert_eq!(unbound["items"].as_array().map(Vec::len), Some(0));
    let (_, all) = world
        .call(Method::GET, "/api/v1/provider-accounts", &reader, None)
        .await;
    assert_eq!(all["items"][0]["account"], json!(checking.to_string()));

    let again = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .err()
        .unwrap();
    assert!(matches!(again, SyncError::Conflict(_)));
}

/// A provider account's balance reaches its account when it is bound, not
/// only at the next sync: net worth would miss the account until then.
#[tokio::test]
async fn bind_brings_the_reported_balance() {
    let world = world().await;
    let on = date(2026, 10, 30);
    let mut with_balance = reported("acct-1", "USD", None, Vec::new());
    with_balance.balance = Some((on, DecimalAmount::parse("23631.9805").unwrap()));
    let without_balance = reported("acct-2", "USD", Some(AccountKind::Savings), Vec::new());
    let mut created = reported("acct-3", "USD", Some(AccountKind::Savings), Vec::new());
    created.balance = Some((on, DecimalAmount::parse("-12.50").unwrap()));
    world
        .sync_once(vec![with_balance, without_balance, created])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;

    world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();

    let writer = world.token(Scope::Write).await;
    let (status, account) = world
        .call(
            Method::GET,
            &format!("/api/v1/accounts/{checking}"),
            &writer,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        account["latest_balance"]["balance"]["minor_units"],
        json!(2_363_198)
    );
    assert_eq!(account["latest_balance"]["on"], json!("2026-10-30"));
    assert_eq!(account["latest_balance"]["source"], json!("sync"));

    let other = world.provider_account("acct-2").await;
    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", other.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        linked["account"]["latest_balance"],
        json!(null),
        "no balance was reported, so none is invented"
    );

    let created = world.provider_account("acct-3").await;
    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", created.id),
            &writer,
            Some(json!({"name": "Linked savings"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        linked["account"]["latest_balance"]["balance"]["minor_units"],
        json!(-1250)
    );
}

/// A provider's balance dated in the future would stay the account's latest
/// until that day, hiding every balance later syncs write.
#[tokio::test]
async fn bind_dates_a_future_balance_today() {
    let world = world().await;
    let mut future = reported("acct-1", "USD", None, Vec::new());
    future.balance = Some((date(2099, 1, 1), DecimalAmount::parse("10.00").unwrap()));
    world.sync_once(vec![future]).await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;

    world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .unwrap();

    let writer = world.token(Scope::Write).await;
    let (_, account) = world
        .call(
            Method::GET,
            &format!("/api/v1/accounts/{checking}"),
            &writer,
            None,
        )
        .await;
    let today = world
        .server
        .clock
        .now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    assert_eq!(account["latest_balance"]["on"], json!(today));
    assert_eq!(
        account["latest_balance"]["balance"]["minor_units"],
        json!(1000)
    );
}

#[tokio::test]
async fn create_account_and_bind_via_api() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Savings),
            vec![txn("t-1", "25.00")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let writer = world.token(Scope::Write).await;

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(linked["account"]["kind"], json!("savings"));
    assert_eq!(linked["account"]["name"], json!("Everyday savings"));
    assert_eq!(linked["account"]["currency"], json!("USD"));
    assert_eq!(linked["account"]["source"], json!("linked"));
    assert_eq!(linked["account"]["liquidity"], json!("cash"));
    assert_eq!(
        linked["provider_account"]["account"],
        linked["account"]["id"]
    );
    let account: AccountId = linked["account"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(world.transactions(account).await.len(), 1);

    // A manual account created through the API, for the audit's shape.
    let (status, manual) = world
        .call(
            Method::POST,
            "/api/v1/accounts",
            &writer,
            Some(json!({"name": "Manual", "kind": "savings", "currency": "USD"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let creates = world.audit_actions("account.create").await;
    let linked_create = creates
        .iter()
        .find(|event| event.entity_id == account.to_string())
        .unwrap();
    let manual_create = creates
        .iter()
        .find(|event| Some(event.entity_id.as_str()) == manual["id"].as_str())
        .unwrap();
    let fields = |event: &AuditEvent| -> Vec<String> {
        event.changes.iter().map(|(f, _)| f.to_owned()).collect()
    };
    assert_eq!(fields(linked_create), fields(manual_create));
    assert_eq!(linked_create.entity_type, "account");
    assert_eq!(linked_create.actor, manual_create.actor);
    assert!(matches!(
        linked_create.actor,
        AuditActor::Member { agent: Some(_), .. }
    ));
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 1);

    // Bound already: refused, and no second account is created.
    let accounts_before = world
        .server
        .store
        .list_accounts(&AccountScope::All, true)
        .await
        .unwrap()
        .len();
    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("conflict"));
    let accounts_after = world
        .server
        .store
        .list_accounts(&AccountScope::All, true)
        .await
        .unwrap()
        .len();
    assert_eq!(accounts_after, accounts_before);
}

#[tokio::test]
async fn create_account_needs_a_kind_and_a_known_currency() {
    let world = world().await;
    world
        .sync_once(vec![
            reported("acct-plain", "USD", None, Vec::new()),
            reported("acct-odd", "DOGE", Some(AccountKind::Checking), Vec::new()),
        ])
        .await;
    let plain = world.provider_account("acct-plain").await;
    let odd = world.provider_account("acct-odd").await;
    let writer = world.token(Scope::Write).await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", plain.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["code"], json!("invalid"));
    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", odd.id),
            &writer,
            Some(json!({"name": "Odd"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], json!("currency_mismatch"));

    let (status, linked) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", plain.id),
            &writer,
            Some(json!({"name": "Joint checking", "kind": "checking"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(linked["account"]["name"], json!("Joint checking"));
    assert_eq!(linked["account"]["kind"], json!("checking"));
    assert_eq!(world.audit_actions("account.create").await.len(), 1);
}

#[tokio::test]
async fn read_token_cannot_bind() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Checking),
            vec![txn("t-1", "-4.50")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    let reader = world.token(Scope::Read).await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/bind", held.id),
            &reader,
            Some(json!({"account": checking})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], json!("forbidden"));
    let (status, _) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &reader,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    assert_eq!(world.provider_account("acct-1").await.account, None);
    assert_eq!(world.transactions(checking).await.len(), 0);
    assert_eq!(world.audit_actions("provider_account.bind").await.len(), 0);

    // The same request with a write token binds.
    let writer = world.token(Scope::Write).await;
    let (status, bound) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/bind", held.id),
            &writer,
            Some(json!({"account": checking})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bound["account"], json!(checking.to_string()));
}

#[tokio::test]
async fn bind_needs_write_on_account() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    let reader = Actor {
        member: world.alice.member,
        role: Role::Admin,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Read,
        },
    };
    assert!(matches!(
        world.server.access.resolve(&reader).await,
        Ok(AccountAccess::Uniform(AccountLevel::Transactions))
    ));

    let err = world
        .server
        .sync
        .bind(&reader, held.id, checking)
        .await
        .err()
        .unwrap();
    assert!(denied(
        &err,
        Action::AccessAccount {
            needed: AccountLevel::Write
        },
        DenyReason::AccountLevel {
            needed: AccountLevel::Write
        },
    ));
    assert_eq!(world.provider_account("acct-1").await.account, None);
}

#[tokio::test]
async fn bind_refuses_unknown_and_removed() {
    let world = world().await;
    world
        .sync_once(vec![reported("acct-1", "USD", None, Vec::new())])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;

    let err = world
        .server
        .sync
        .bind(&world.alice, ProviderAccountId::new(), checking)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::NotFound));
    let err = world
        .server
        .sync
        .bind(&world.alice, held.id, AccountId::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::NotFound));

    world
        .server
        .sync
        .remove_connection(&world.alice, world.connection)
        .await
        .unwrap();
    let err = world
        .server
        .sync
        .bind(&world.alice, held.id, checking)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SyncError::Conflict(_)));
    assert_eq!(world.provider_account("acct-1").await.account, None);
}

#[tokio::test]
async fn bind_to_a_closed_account_is_refused() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            None,
            vec![txn("t-1", "-4.50")],
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let checking = world.account("Checking", "USD").await;
    world
        .server
        .ledger
        .update_account(
            &world.alice,
            checking,
            coffer_server::ledger::accounts::AccountPatch {
                closed: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let writer = world.token(Scope::Write).await;
    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/bind", held.id),
            &writer,
            Some(json!({"account": checking})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"].as_str(), Some("conflict"));
    assert_eq!(world.provider_account("acct-1").await.account, None);
    assert_eq!(world.transactions(checking).await.len(), 0);
}

/// Two provider accounts of one connection cannot feed the same account
/// (every record would arrive twice); another connection's binding to it
/// is allowed.
#[tokio::test]
async fn second_binding_from_the_same_connection_is_refused() {
    let world = world().await;
    world
        .sync_once(vec![
            reported("acct-old", "USD", None, vec![txn("t-1", "-4.50")]),
            reported("acct-new", "USD", None, vec![txn("t-2", "-4.50")]),
        ])
        .await;
    let checking = world.account("Checking", "USD").await;
    let writer = world.token(Scope::Write).await;
    let bind = |provider_account: ProviderAccountId| {
        let writer = &writer;
        let world = &world;
        async move {
            world
                .call(
                    Method::POST,
                    &format!("/api/v1/provider-accounts/{provider_account}/bind"),
                    writer,
                    Some(json!({"account": checking})),
                )
                .await
        }
    };
    let old = world.provider_account("acct-old").await;
    assert_eq!(bind(old.id).await.0, StatusCode::OK);

    let new = world.provider_account("acct-new").await;
    let (status, problem) = bind(new.id).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"].as_str(), Some("conflict"));
    assert_eq!(world.provider_account("acct-new").await.account, None);
    assert_eq!(world.transactions(checking).await.len(), 1);

    // A provider account of another connection may still be bound to it.
    let other = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "Other bank",
            world.alice.member,
            PLAINTEXT,
        )
        .await;
    let mut tx = world.server.store.begin().await.unwrap();
    let foreign = tx
        .upsert_provider_account(ProviderAccountUpsert {
            connection: other,
            provider_account_id: "acct-other".into(),
            name: "Other".into(),
            institution: None,
            currency_raw: "USD".into(),
            kind_hint: None,
            balance: None,
            seen_at: world.now(),
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(bind(foreign.id).await.0, StatusCode::OK);
}

/// Without a name from the caller or a usable one from the provider, the
/// request is refused with a message that asks for a name.
#[tokio::test]
async fn create_account_without_a_usable_name_asks_for_one() {
    let world = world().await;
    let mut nameless = reported("acct-1", "USD", Some(AccountKind::Checking), Vec::new());
    nameless.name = "\u{200b}".into();
    world.sync_once(vec![nameless]).await;
    let held = world.provider_account("acct-1").await;
    let writer = world.token(Scope::Write).await;

    let (status, problem) = world
        .call(
            Method::POST,
            &format!("/api/v1/provider-accounts/{}/create-account", held.id),
            &writer,
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let asks_for_name = problem["detail"]
        .as_str()
        .is_some_and(|detail| detail.starts_with("name is required"));
    assert!(asks_for_name);
    assert_eq!(world.audit_actions("account.create").await.len(), 0);
}

/// An invalid name is refused before the write lock is taken: with another
/// writer holding it, the refusal still comes at once.
#[tokio::test]
async fn invalid_names_are_refused_before_taking_the_write_lock() {
    let world = world().await;
    world
        .sync_once(vec![reported(
            "acct-1",
            "USD",
            Some(AccountKind::Checking),
            Vec::new(),
        )])
        .await;
    let held = world.provider_account("acct-1").await;
    let writer = world.server.store.begin().await.unwrap();

    let created = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        world.server.ledger.create_account(
            &world.alice,
            NewAccountInput {
                name: String::new(),
                kind: AccountKind::Checking,
                currency: Currency::parse("USD").unwrap(),
                liquidity: None,
                include_in_net_worth: true,
                include_in_forecast: true,
                opening_balance: None,
            },
            None,
        ),
    )
    .await;
    let refused_at_once = matches!(
        created,
        Ok(Err(coffer_server::ledger::LedgerError::Invalid(_)))
    );
    assert!(refused_at_once);

    let linked = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        world.server.sync.create_account_and_bind(
            &world.alice,
            held.id,
            coffer_server::sync::connections::NewLinkedAccountInput {
                name: Some(String::new()),
                kind: None,
                liquidity: None,
                include_in_net_worth: true,
                include_in_forecast: true,
            },
        ),
    )
    .await;
    let refused_at_once = matches!(linked, Ok(Err(SyncError::Invalid(_))));
    assert!(refused_at_once);
    drop(writer);
}
