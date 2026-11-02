//! What a forecast reads from the store (spec §8.9 R9, R10, R19): the balance it
//! takes, the spending that balance already shows, and an order that doesn't change.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod goals;
mod reads;

use coffer_domain::accounts::{AccountKind, AccountName, BalanceSource, BelongsTo, LiquidityClass};
use coffer_domain::ids::{AccountId, CategoryId, PlannedItemId};
use coffer_domain::money::Currency;
use coffer_domain::plan::PlanSchedule;
use coffer_domain::transactions::{
    CategoryGroupName, CategoryKind, CategoryName, Description, SplitInput, TransactionOrigin,
};
use coffer_store::{
    Attribution, BalanceSnapshot, NewAccount, NewCategory, NewCategoryGroup, NewTransaction,
};
use jiff::civil::{Date, date};

use super::plan::{World, at};

const AS_OF: Date = date(2026, 11, 15);

/// A transaction flag a sync sets, planted here to pin what a read does with it.
#[derive(Clone, Copy)]
enum Flag {
    SourceRemoved,
    SplitMismatch,
    PossibleDuplicate,
}

impl Flag {
    fn statement(self) -> &'static str {
        match self {
            Self::SourceRemoved => {
                "UPDATE transactions SET flag_source_removed = 1 WHERE amount_minor = ?"
            }
            Self::SplitMismatch => {
                "UPDATE transactions SET flag_split_mismatch = 1 WHERE amount_minor = ?"
            }
            Self::PossibleDuplicate => {
                "UPDATE transactions SET flag_possible_duplicate = 1 WHERE amount_minor = ?"
            }
        }
    }
}

/// How a test's account counts towards the forecast.
fn cash_account(name: &str) -> NewAccount {
    NewAccount {
        id: AccountId::new(),
        name: AccountName::parse(name).unwrap(),
        kind: AccountKind::Checking,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Cash,
        liquidity_overridden: false,
        include_in_net_worth: true,
        include_in_forecast: true,
        created_at: at(0),
        belongs_to: BelongsTo::Household,
        belongs_to_overridden: false,
        beneficiary: None,
    }
}

impl World {
    async fn forecast_account(&self, new: NewAccount) -> AccountId {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx.insert_account(new).await.unwrap();
        tx.commit().await.unwrap();
        record.id
    }

    async fn balance(&self, account: AccountId, on: Date, balance_minor: i64) {
        let mut tx = self.store.begin().await.unwrap();
        tx.upsert_balance(BalanceSnapshot {
            account,
            on,
            balance_minor,
            source: BalanceSource::Manual,
            recorded_at: at(0),
        })
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// A transaction of `minor_units`, posted on `on`, with one split of all of it in
    /// `category`, or none without one.
    async fn transaction(
        &self,
        account: AccountId,
        on: Date,
        minor_units: i64,
        split: Option<(CategoryId, Option<PlannedItemId>)>,
        pending: bool,
    ) {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Manual,
                account,
                posted_on: on,
                transacted_on: None,
                pending,
                amount_minor: minor_units,
                currency: Currency::parse("USD").unwrap(),
                raw_description: Description::parse("Purchase").unwrap(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        if let Some((category, goal)) = split {
            tx.replace_splits(
                record.id,
                &[SplitInput {
                    category,
                    minor_units,
                    goal,
                }],
                Attribution {
                    member: None,
                    agent: None,
                    system: Some("test".into()),
                    at: at(0),
                },
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// A new category of `kind`.
    async fn category_of(&self, kind: CategoryKind) -> CategoryId {
        let mut tx = self.store.begin().await.unwrap();
        let group = tx
            .insert_category_group(NewCategoryGroup {
                name: CategoryGroupName::parse("Other").unwrap(),
                position: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        let category = tx
            .insert_category(NewCategory {
                group: group.id,
                name: CategoryName::parse("Other").unwrap(),
                kind,
                position: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        category.id
    }

    /// Sets `flag` on the transactions of exactly `minor_units`.
    async fn flag(&self, flag: Flag, minor_units: i64) {
        let pool = self.pool().await;
        sqlx::query(flag.statement())
            .bind(minor_units)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// A transaction in `currency` split as `splits` say.
    async fn split_transaction(
        &self,
        account: AccountId,
        on: Date,
        currency: &str,
        splits: &[SplitInput],
    ) {
        let mut tx = self.store.begin().await.unwrap();
        let record = tx
            .insert_transaction(NewTransaction {
                origin: TransactionOrigin::Manual,
                account,
                posted_on: on,
                transacted_on: None,
                pending: false,
                amount_minor: splits.iter().map(|split| split.minor_units).sum(),
                currency: Currency::parse(currency).unwrap(),
                raw_description: Description::parse("Purchase").unwrap(),
                imported_payee: None,
                memo: None,
                notes: None,
                created_at: at(0),
            })
            .await
            .unwrap();
        tx.replace_splits(
            record.id,
            splits,
            Attribution {
                member: None,
                agent: None,
                system: Some("test".into()),
                at: at(0),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    /// `AS_OF`'s spending newer than balances, as (currency, total, pending).
    async fn newer_spending(&self) -> Vec<(String, i64, i64)> {
        let data = self.store.forecast_data(AS_OF).await.unwrap();
        data.newer_spending
            .iter()
            .map(|row| {
                (
                    row.currency.as_str().to_owned(),
                    row.total_minor,
                    row.pending_minor,
                )
            })
            .collect()
    }

    async fn spend(&self, account: AccountId, on: Date, minor_units: i64) {
        let split = Some((self.category, None));
        self.transaction(account, on, minor_units, split, false)
            .await;
    }

    /// The goal `transaction` calls link to.
    async fn goal(&self) -> PlannedItemId {
        self.insert(self.new_item(PlanSchedule::Goal {
            start_on: date(2026, 1, 1),
            due_on: date(2027, 1, 1),
            completed: false,
        }))
        .await
    }

    async fn spend_on_goal(&self, account: AccountId, on: Date, goal: PlannedItemId, minor: i64) {
        let split = Some((self.category, Some(goal)));
        self.transaction(account, on, minor, split, false).await;
    }

    /// What `AS_OF`'s forecast reads as spent in the world's category.
    async fn category_spent(&self) -> Vec<i64> {
        let data = self.store.forecast_data(AS_OF).await.unwrap();
        data.category_spent
            .iter()
            .map(|row| row.spent_minor)
            .collect()
    }

    async fn goal_spent(&self, goal: PlannedItemId) -> i64 {
        let data = self.store.forecast_data(AS_OF).await.unwrap();
        data.goal_spent
            .iter()
            .find(|(found, _)| *found == goal)
            .map_or(0, |(_, spent)| *spent)
    }
}
