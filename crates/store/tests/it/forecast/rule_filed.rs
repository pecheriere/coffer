//! What a forecast reads of transfers a rule filed: only a rule's, only money out, only
//! this month, by currency.

use coffer_domain::forecast::RuleFiledTransfers;
use coffer_domain::ids::{AccountId, CategoryId, RuleId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::transactions::{CategoryKind, SplitInput};
use coffer_store::Attribution;
use jiff::civil::{Date, date};

use super::super::plan::{at, world};
use super::{AS_OF, World};

fn usd(minor_units: i64) -> Money {
    Money {
        minor_units,
        currency: Currency::parse("USD").unwrap(),
    }
}

impl World {
    /// A USD transaction of `minor_units` filed under `category`, with a rule named as having
    /// chosen it or without one.
    async fn filed(
        &self,
        account: AccountId,
        on: Date,
        minor_units: i64,
        category: CategoryId,
        by_rule: bool,
    ) {
        let rule = by_rule.then(RuleId::new);
        let attribution = Attribution {
            member: None,
            agent: None,
            system: Some("test".into()),
            rule,
            at: at(0),
        };
        self.split_filed(
            account,
            on,
            &[SplitInput {
                category,
                minor_units,
                goal: None,
            }],
            attribution,
        )
        .await;
    }

    async fn rule_filed_transfers(&self) -> Vec<RuleFiledTransfers> {
        self.store
            .forecast_data(AS_OF)
            .await
            .unwrap()
            .rule_filed_transfers
    }
}

#[tokio::test]
async fn a_rule_filing_money_out_as_transfer_is_counted() {
    let world = world().await;
    let transfer = world.category_of(CategoryKind::Transfer).await;
    world
        .filed(world.account, date(2026, 11, 3), -4_000, transfer, true)
        .await;
    world
        .filed(world.account, date(2026, 11, 9), -1_500, transfer, true)
        .await;

    assert_eq!(
        world.rule_filed_transfers().await,
        [RuleFiledTransfers {
            currency: Currency::parse("USD").unwrap(),
            count: 2,
            amount: usd(5_500),
        }]
    );
}

#[tokio::test]
async fn what_no_rule_filed_money_in_another_kind_or_month_is_not() {
    let world = world().await;
    let transfer = world.category_of(CategoryKind::Transfer).await;
    world
        .filed(world.account, date(2026, 11, 3), -4_000, transfer, false)
        .await;
    world
        .filed(world.account, date(2026, 11, 4), 2_000, transfer, true)
        .await;
    world
        .filed(world.account, date(2026, 11, 5), -900, world.category, true)
        .await;
    world
        .filed(world.account, date(2026, 10, 30), -700, transfer, true)
        .await;
    world
        .filed(world.account, date(2026, 11, 16), -600, transfer, true)
        .await;

    assert!(world.rule_filed_transfers().await.is_empty());
}
