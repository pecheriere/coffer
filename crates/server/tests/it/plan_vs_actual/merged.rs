//! A merged duplicate is one transaction again (spec §8.4): the report counts it once,
//! and stops saying it may be a duplicate.

use axum::http::Method;
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::ids::{AccountId, TransactionId};
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::TransactionFlag;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_store::{NewConnection, ProviderAccountUpsert};
use jiff::civil::date;
use serde_json::{Value, json};

use super::{TRANSACTIONS, World, row, usd, world};

impl World {
    /// A connection of its own reporting one charge of 1,250 on `day`: charges one
    /// connection reports apart can't be merged.
    async fn imported(&self, checking: AccountId, day: i8) {
        let account = self.server.store.get_account(checking).await.unwrap();
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        let connection = tx
            .insert_connection(NewConnection {
                provider: "testbank",
                label: ConnectionLabel::parse("Bank").unwrap(),
                owner: self.alice.member,
                credentials: vec![1, 2, 3],
                created_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection: connection.id,
                provider_account_id: "one".into(),
                name: "Everyday".into(),
                institution: None,
                currency_raw: "USD".into(),
                kind_hint: Some(AccountKind::Checking),
                balance: None,
                seen_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx
            .bind_provider_account(provider_account.id, checking)
            .await
            .unwrap();
        apply_account_batch(
            &mut tx,
            now,
            connection.id,
            AccountBatch {
                provider_account,
                account: Some(account),
                transactions: vec![NormalizedTransaction {
                    provider_id: "charge".into(),
                    posted_on: date(2026, 10, day),
                    date_reported: true,
                    transacted_on: None,
                    amount_minor: -1_250,
                    amount_raw: "-1250".into(),
                    description: "SHOP".into(),
                    payee: None,
                    memo: None,
                    pending: false,
                    replaces_provider_id: None,
                }],
                removed: Vec::new(),
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn a_merged_duplicate_counts_once() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    // Far enough apart that a sync doesn't match them itself.
    world.imported(checking, 1).await;
    world.imported(checking, 28).await;
    let (_, listed) = world
        .send(Method::GET, TRANSACTIONS, &world.admin, None)
        .await;
    let listed: Value = serde_json::from_str(&listed).unwrap();
    let first: TransactionId = serde_json::from_value(listed["items"][0]["id"].clone()).unwrap();
    let second: TransactionId = serde_json::from_value(listed["items"][1]["id"].clone()).unwrap();
    let mut tx = world.server.store.begin().await.unwrap();
    tx.set_flag(second, TransactionFlag::PossibleDuplicate, true)
        .await
        .unwrap();
    tx.add_duplicate_candidates(second, &[first]).await.unwrap();
    tx.commit().await.unwrap();
    for id in [first, second] {
        world
            .admin_send(
                Method::PATCH,
                &format!("{TRANSACTIONS}/{id}"),
                &json!({"splits": [{"category": food, "minor_units": -1_250}]}),
            )
            .await;
    }
    let before = world.october().await;
    let flagged = row(&before, "2026-10", food);
    assert_eq!(flagged["actual"], usd(2_500), "both, until it's settled");
    assert_eq!(flagged["possible_duplicate"], usd(1_250));

    world
        .admin_send(
            Method::POST,
            &format!("{TRANSACTIONS}/{second}/merge"),
            &json!({"duplicate": first, "discard_duplicate_state": true}),
        )
        .await;

    let after = world.october().await;
    let merged = row(&after, "2026-10", food);
    assert_eq!(merged["actual"], usd(1_250));
    assert_eq!(merged["possible_duplicate"], usd(0));
}
