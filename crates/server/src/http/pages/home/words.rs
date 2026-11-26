//! What Home says about free money, in plain words. A number with no figure of its own
//! (a blocked currency) gets no sentence here: the card says why instead.

use coffer_domain::forecast::FreeMoneyCurrency;

use crate::http::pages::amounts::{magnitude, money};

/// The answer in a sentence: how much is free, or how far short the cash falls of what is
/// spoken for. `None` for a currency a blocker leaves without a number.
pub(super) fn hero_sentence(row: &FreeMoneyCurrency) -> Option<String> {
    let free = row.free_money?;
    let short = row.shortfall.filter(|short| short.minor_units > 0);
    Some(match short {
        Some(short) => format!("You're {} short of what's spoken for", magnitude(short)),
        None => format!("{} free", money(free)),
    })
}

/// What the emergency fund's amount is measured against: its target, or that there is none.
pub(super) fn held_note(row: &FreeMoneyCurrency) -> String {
    if row.emergency_fund.is_none() {
        return "none set".to_owned();
    }
    format!("held, of {}", money(row.emergency_fund_target))
}

#[cfg(test)]
mod tests {
    use coffer_domain::forecast::{FreeMoneyCurrency, StillNeed};
    use coffer_domain::money::{Currency, Money};
    use coffer_domain::plan::EmergencyFundTarget;

    use super::{held_note, hero_sentence};

    fn usd(minor_units: i64) -> Money {
        Money {
            minor_units,
            currency: Currency::parse("USD").unwrap(),
        }
    }

    fn row() -> FreeMoneyCurrency {
        FreeMoneyCurrency {
            currency: Currency::parse("USD").unwrap(),
            cash_available: usd(1_000_000),
            still_need: StillNeed {
                must_pay_this_month: usd(0),
                must_pay_coming: usd(0),
                essentials: usd(0),
                flexible: usd(0),
                total: usd(0),
            },
            set_aside: usd(0),
            emergency_fund: Some(EmergencyFundTarget::Fixed {
                minor_units: 300_000,
            }),
            emergency_fund_target: usd(300_000),
            emergency_fund_held: Some(usd(300_000)),
            free_money: Some(usd(385_000)),
            shortfall: Some(usd(0)),
        }
    }

    #[test]
    fn free_money_is_said_with_its_currency() {
        assert_eq!(hero_sentence(&row()).as_deref(), Some("3,850.00 USD free"));
    }

    #[test]
    fn a_shortfall_says_how_far_short() {
        let mut short = row();
        short.free_money = Some(usd(0));
        short.shortfall = Some(usd(100_000));
        assert_eq!(
            hero_sentence(&short).as_deref(),
            Some("You're 1,000.00 USD short of what's spoken for")
        );
    }

    #[test]
    fn a_blocked_currency_has_no_sentence() {
        let mut blocked = row();
        blocked.free_money = None;
        blocked.shortfall = None;
        assert_eq!(hero_sentence(&blocked), None);
    }

    #[test]
    fn the_emergency_fund_says_held_of_target_or_that_there_is_none() {
        assert_eq!(held_note(&row()), "held, of 3,000.00 USD");
        let mut none = row();
        none.emergency_fund = None;
        assert_eq!(held_note(&none), "none set");
    }
}
