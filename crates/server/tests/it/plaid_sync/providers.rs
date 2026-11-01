//! Plaid next to the other providers: registration and alternating runs.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use super::{page, record, world};
use coffer_domain::money::DecimalAmount;
use coffer_server::config::Config;
use coffer_server::egress::Egress;
use coffer_server::sync::apply::Coverage;
use coffer_server::sync::provider::{
    FetchMode, FetchResult, ProviderAccountData, ProviderKind, ProviderRegistry,
    ProviderTransaction,
};
use jiff::SignedDuration;
use jiff::civil::date;
use secrecy::SecretString;
use serde_json::json;

/// Plaid is offered exactly when it is configured, with its documented
/// limits.
#[tokio::test]
async fn registered_only_when_configured() {
    let world = world().await;
    let config = Config {
        plaid: None,
        ..world.server.config.clone()
    };
    let without = ProviderRegistry::from_config(&config, &Egress::for_tests()).unwrap();
    assert!(without.get(ProviderKind::Plaid).is_none());

    let config = Config {
        plaid: Some(coffer_server::config::PlaidConfig {
            client_id: "id".to_owned(),
            secret: SecretString::from("secret"),
            environment: coffer_server::config::PlaidEnvironment::Sandbox,
            country_codes: vec!["US".to_owned()],
        }),
        ..world.server.config.clone()
    };
    let with = ProviderRegistry::from_config(&config, &Egress::for_tests()).unwrap();
    let plaid = with.get(ProviderKind::Plaid).unwrap();
    assert_eq!(plaid.kind(), ProviderKind::Plaid);
    let capabilities = plaid.capabilities();
    assert!(capabilities.pending);
    assert_eq!(capabilities.mode, FetchMode::Cursor);
    assert_eq!(capabilities.min_interval, SignedDuration::from_mins(15));
    assert_eq!(
        capabilities.min_scheduled_interval,
        SignedDuration::from_hours(1)
    );
    assert_eq!(capabilities.daily_budget, 48);
}

fn simplefin_record(description: &str) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: "sf-1".into(),
        posted_on: Some(date(2027, 1, 10)),
        transacted_on: None,
        amount: DecimalAmount::parse("-1").unwrap(),
        amount_raw: "-1".to_owned(),
        description: description.into(),
        payee: None,
        memo: None,
        pending: false,
        replaces_provider_id: None,
    }
}

fn simplefin_answer(description: &str) -> FetchResult {
    FetchResult {
        accounts: vec![ProviderAccountData {
            provider_account_id: "sf-account".into(),
            name: "SimpleFin account".into(),
            institution: None,
            currency: "USD".into(),
            kind_hint: None,
            balance: None,
            transactions: vec![simplefin_record(description)],
            removed: Vec::new(),
            coverage: Coverage::Delta,
            error: None,
            unusable: Vec::new(),
        }],
        connection_errors: Vec::new(),
        cursor: None,
    }
}

/// One purchase reported by Plaid and by `SimpleFin` under different
/// descriptions: the provider that reported it first keeps owning the
/// imported fields, whatever the other sends, and alternating syncs write
/// nothing to the transaction.
#[tokio::test]
async fn alternating_providers_write_nothing_after_the_first() {
    let world = world().await;
    let account = world.bound("acct-checking").await;
    let simplefin = world
        .server
        .seed_connection(
            ProviderKind::SimpleFin,
            "SimpleFin bank",
            world.alice.member,
            "https://u:p@bridge.example/simplefin",
        )
        .await;
    world.bind_to(simplefin, "sf-account", account).await;

    world.fake.push(Ok(simplefin_answer("ACME STORE 123")));
    world.sync_connection(simplefin).await;
    let mut plaid_record = record("pl-1", "acct-checking", "1");
    plaid_record["name"] = json!("Acme Store Purchase");
    world.script.page(page(
        std::slice::from_ref(&plaid_record),
        &[],
        &[],
        "c1",
        false,
    ));
    world.sync().await;

    let settled = world.transactions(account).await.remove(0);
    assert_eq!(
        settled.sources.len(),
        2,
        "both records hold one transaction"
    );
    assert_eq!(settled.raw_description.as_str(), "ACME STORE 123");
    let updates = world.imported_updates().await;

    for round in 0..3 {
        world.fake.push(Ok(simplefin_answer("ACME STORE 123")));
        world.sync_connection(simplefin).await;
        let mut changed = plaid_record.clone();
        changed["name"] = json!(format!("Acme Store Purchase {round}"));
        world
            .script
            .page(page(&[], std::slice::from_ref(&changed), &[], "c2", false));
        world.sync().await;
    }

    let after = world.transactions(account).await.remove(0);
    assert_eq!(after.raw_description, settled.raw_description);
    assert_eq!(
        after.updated_at, settled.updated_at,
        "nothing was rewritten"
    );
    assert_eq!(
        world.imported_updates().await,
        updates,
        "nothing was audited"
    );
    assert_eq!(world.transactions(account).await.len(), 1);
}
