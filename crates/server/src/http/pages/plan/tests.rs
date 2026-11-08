//! The plan page's content as someone who may read the plan but not change it sees it. Every
//! signed-in member may change the plan today, so the router can't show this one.

use std::collections::HashMap;

use coffer_domain::ids::{CategoryId, PlannedItemId};
use coffer_domain::money::{Currency, Money};
use coffer_domain::plan::{IntervalUnit, PlanDirection, PlanSchedule, PlanSpending, PlanTier};
use jiff::Timestamp;
use jiff::civil::date;

use super::form::Choices;
use super::{Names, content};
use crate::ledger::plan_items::PlannedItemView;
use crate::themes::Look;

fn budget(name: &str, tier: PlanTier) -> PlannedItemView {
    PlannedItemView {
        id: PlannedItemId::new(),
        name: name.to_owned(),
        category: CategoryId::new(),
        amount: Money {
            minor_units: 40_000,
            currency: Currency::parse("USD").expect("a known currency"),
        },
        direction: PlanDirection::Outflow,
        account_hint: None,
        notes: None,
        active: true,
        schedule: PlanSchedule::Recurring {
            every: 1,
            unit: IntervalUnit::Months,
            anchor_on: date(2026, 11, 1),
            end_on: None,
        },
        spending: Some(PlanSpending::Budget),
        tier: Some(tier),
        settlements: Vec::new(),
        created_at: Timestamp::UNIX_EPOCH,
        updated_at: Timestamp::UNIX_EPOCH,
    }
}

#[test]
fn a_viewer_sees_tiers_but_no_form() {
    let items = [
        budget("Groceries", PlanTier::Essentials),
        budget("Hobbies", PlanTier::Flexible),
    ];
    let names = Names {
        categories: HashMap::new(),
        accounts: HashMap::new(),
    };
    let choices = Choices {
        groups: &[],
        accounts: &[],
        currency: None,
    };

    let page = content(
        Look::default(),
        &items,
        &names,
        &HashMap::new(),
        &choices,
        None,
    )
    .into_string();

    assert!(page.contains(
        "<span class=\"tier-mark tier-essentials\" aria-hidden=\"true\"></span>Essentials</h2>"
    ));
    assert!(page.contains(
        "<span class=\"tier-mark tier-flexible\" aria-hidden=\"true\"></span>Flexible</h2>"
    ));
    assert!(!page.contains("<form"), "no form to change anything");
    assert!(!page.contains("name=\"tier\""), "no tier to choose");
}
