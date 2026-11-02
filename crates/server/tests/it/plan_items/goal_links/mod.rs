//! Splits linked to goals (spec §8.9 R8, R17): what a split may name, that a
//! link survives whatever rewrites its split, what it stops a goal doing, and
//! that a goal the token can't see answers exactly like a missing one.

mod hidden;
mod rewrites;
mod rules;

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::{AccountKind, ConnectionLabel};
use coffer_domain::audit::AuditValue;
use coffer_domain::identity::Scope;
use coffer_domain::ids::{AccountId, AgentTokenId, CategoryId, ConnectionId};
use coffer_domain::policy::AccountLevel::Write;
use coffer_domain::time::Clock as _;
use coffer_domain::transactions::SplitInput;
use coffer_server::secrets::SecretToken;
use coffer_server::sync::apply::{
    AccountBatch, Coverage, NormalizedTransaction, apply_account_batch,
};
use coffer_store::{
    AccountRecord, Attribution, AuditEvent, AuditFilter, NewConnection, PageRequest,
    ProviderAccountRecord, ProviderAccountUpsert,
};
use jiff::civil::date;
use serde_json::{Value, json};

use super::{World, item_uri, on, with};

const TRANSACTIONS: &str = "/api/v1/transactions";
const NO_SUCH_GOAL: &str = "a split names a goal that does not exist";
const HAS_LINKS: &str = "the goal has linked splits; mark it complete instead";

fn transaction_uri(id: &str) -> String {
    format!("{TRANSACTIONS}/{id}")
}

/// One split of `minor_units`, linked to `goal`.
fn linked(category: CategoryId, minor_units: i64, goal: impl serde::Serialize) -> Value {
    json!([{"category": category, "minor_units": minor_units, "goal": goal}])
}

fn euros(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "EUR"})
}

/// A provider account bound to an account of its own.
struct Feed {
    connection: ConnectionId,
    provider_account: ProviderAccountRecord,
    bound: AccountRecord,
}

fn provider_record(id: &str, amount_minor: i64, pending: bool) -> NormalizedTransaction {
    NormalizedTransaction {
        provider_id: id.into(),
        posted_on: date(2026, 10, 10),
        date_reported: true,
        transacted_on: None,
        amount_minor,
        amount_raw: amount_minor.to_string(),
        description: id.into(),
        payee: None,
        memo: None,
        pending,
        replaces_provider_id: None,
    }
}

impl World {
    /// A manual transaction of `minor_units`; returns its id.
    async fn manual(&self, ledger: AccountId, minor_units: i64) -> String {
        let body = json!({
            "account": ledger,
            "posted_on": "2027-01-10",
            "minor_units": minor_units,
            "description": "Purchase",
        });
        let (status, answer) = self
            .call(Method::POST, TRANSACTIONS, &self.admin, Some(&body))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        answer["id"].as_str().unwrap().to_owned()
    }

    /// Sets `splits` on `transaction` as `token`; answers the raw body.
    async fn set_splits(
        &self,
        token: &SecretToken,
        transaction: &str,
        splits: Value,
    ) -> (StatusCode, String) {
        let body = json!({"splits": splits});
        self.send(
            Method::PATCH,
            &transaction_uri(transaction),
            token,
            Some(&body),
            None,
        )
        .await
    }

    /// Links `transaction`'s one split to `goal`, which must succeed.
    async fn link(&self, transaction: &str, minor_units: i64, goal: &str) {
        let splits = linked(self.expense, minor_units, goal);
        let (status, _) = self.set_splits(&self.admin, transaction, splits).await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn read_transaction(&self, id: &str) -> Value {
        let uri = transaction_uri(id);
        let (status, answer) = self.call(Method::GET, &uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::OK);
        answer
    }

    /// The `after` of `field` in `transaction`'s latest audited update.
    async fn audited_after(&self, transaction: &str, field: &str) -> AuditValue {
        self.transaction_audit(transaction)
            .await
            .iter()
            .find(|event| event.action == "transaction.update")
            .and_then(|event| {
                event
                    .changes
                    .iter()
                    .find(|(name, _)| *name == field)
                    .map(|(_, change)| change.after.clone())
            })
            .unwrap()
    }

    /// `transaction`'s audit entries, newest first.
    async fn transaction_audit(&self, transaction: &str) -> Vec<AuditEvent> {
        let filter = AuditFilter {
            entity_type: Some("transaction".into()),
            entity_id: Some(transaction.to_owned()),
            ..AuditFilter::default()
        };
        let page = self
            .server
            .store
            .list_audit(&filter, &PageRequest::default())
            .await;
        let Ok(page) = page else {
            panic!("the audit log could not be read");
        };
        page.items
    }

    async fn delete_item(&self, id: &str) -> (StatusCode, Value) {
        self.call(Method::DELETE, &item_uri(id), &self.admin, None)
            .await
    }

    async fn feed(&self) -> Feed {
        let ledger = self.account("Imported").await;
        let bound = self.server.store.get_account(ledger).await.unwrap();
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
        let upserted = tx
            .upsert_provider_account(ProviderAccountUpsert {
                connection: connection.id,
                provider_account_id: "acct-1".into(),
                name: "Everyday".into(),
                institution: None,
                currency_raw: "USD".into(),
                kind_hint: Some(AccountKind::Checking),
                balance: None,
                seen_at: now,
            })
            .await
            .unwrap();
        let provider_account = tx.bind_provider_account(upserted.id, ledger).await.unwrap();
        tx.commit().await.unwrap();
        Feed {
            connection: connection.id,
            provider_account,
            bound,
        }
    }

    /// Runs a sync's apply step over `records`.
    async fn apply(&self, feed: &Feed, records: Vec<NormalizedTransaction>) {
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        apply_account_batch(
            &mut tx,
            now,
            feed.connection,
            AccountBatch {
                provider_account: feed.provider_account.clone(),
                account: Some(feed.bound.clone()),
                transactions: records,
                removed: Vec::new(),
                coverage: Coverage::Delta,
                unusable: Vec::new(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// The id of the transaction described as `description` in `feed`.
    async fn imported(&self, feed: &Feed, description: &str) -> String {
        let uri = format!("{TRANSACTIONS}?account={}", feed.bound.id);
        let (status, answer) = self.call(Method::GET, &uri, &self.admin, None).await;
        assert_eq!(status, StatusCode::OK);
        answer["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["description"] == description)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// A transaction linked to a goal hinted at a ledger of its own, and a token
/// with `write` on the transaction's ledger that can't see the goal.
struct HiddenLink {
    home: AccountId,
    transaction: String,
    goal_id: String,
    token: SecretToken,
    token_id: AgentTokenId,
}

impl World {
    async fn hidden_link(&self, item: Value) -> HiddenLink {
        let (home, hidden) = (self.account("Home").await, self.account("Hidden").await);
        let goal_id = self
            .create(&with(item, "account_hint", json!(hidden)))
            .await;
        let transaction = self.manual(home, -500).await;
        self.force_link(&transaction, -500, &goal_id).await;
        let (token_id, token) = self.token(Scope::Write, vec![on(home, Write)]).await;
        HiddenLink {
            home,
            transaction,
            goal_id,
            token,
            token_id,
        }
    }

    /// The splits of `parts`, each a part and its goal, on the expense category.
    fn parts(&self, parts: &[(i64, Option<&str>)]) -> Value {
        parts
            .iter()
            .map(|(minor_units, goal)| match goal {
                Some(goal) => {
                    json!({"category": self.expense, "minor_units": minor_units, "goal": goal})
                }
                None => json!({"category": self.expense, "minor_units": minor_units}),
            })
            .collect()
    }

    /// Writes one linked split straight to the store; see [`Self::force_splits`].
    async fn force_link(&self, transaction: &str, minor_units: i64, goal: &str) {
        self.force_splits(transaction, &[(minor_units, Some(goal))])
            .await;
    }

    /// Writes `parts` straight to the store, past the ledger's checks: the only
    /// way to a link the checks would now refuse, and one with no audit entry.
    async fn force_splits(&self, transaction: &str, parts: &[(i64, Option<&str>)]) {
        let splits: Vec<SplitInput> = parts
            .iter()
            .map(|(minor_units, goal)| SplitInput {
                category: self.expense,
                minor_units: *minor_units,
                goal: goal.map(|goal| goal.parse().unwrap()),
            })
            .collect();
        let now = self.server.clock.now();
        let mut tx = self.server.store.begin().await.unwrap();
        tx.replace_splits(
            transaction.parse().unwrap(),
            &splits,
            Attribution {
                member: Some(self.alice.member),
                agent: None,
                system: None,
                at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}
