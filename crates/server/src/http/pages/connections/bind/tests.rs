//! Who is offered a form. Every session writes today, so a page can't be reached by an actor who
//! can't, and these say what happens if one ever can: the API's tokens hold such access.

use std::collections::{HashMap, HashSet};

use coffer_domain::accounts::{AccountKind, BelongsTo, LiquidityClass};
use coffer_domain::identity::{Actor, Role, Scope, Via};
use coffer_domain::ids::{AccountId, AgentTokenId, ConnectionId, MemberId, ProviderAccountId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant, account_access_with_grants};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::super::{Shared, belonging};
use super::Binding;
use crate::ledger::accounts::{AccountSource, AccountView};
use crate::sync::connections::{ConnectionState, ConnectionView, ProviderAccountView};

fn view() -> AccountView {
    AccountView {
        id: AccountId::new(),
        name: "Plan".to_owned(),
        kind: AccountKind::Education,
        currency: Currency::parse("USD").unwrap(),
        liquidity: LiquidityClass::Invested,
        liquidity_overridden: false,
        include_in_net_worth: false,
        include_in_forecast: false,
        source: AccountSource::Linked,
        closed: false,
        latest_balance: None,
        created_at: Timestamp::UNIX_EPOCH,
        belongs_to: BelongsTo::SomeoneElse,
        belongs_to_overridden: false,
        beneficiary: Some("Child A".to_owned()),
    }
}

/// The row of a provider account bound to `bound`, as `binding` draws it.
fn drawn(binding: &Binding, bound: &AccountView) -> String {
    let id = ConnectionId::new();
    let row = ProviderAccountView {
        id: ProviderAccountId::new(),
        connection: id,
        name: "Plan".to_owned(),
        institution: None,
        currency: "USD".to_owned(),
        kind_hint: None,
        suggested_kind: None,
        account: Some(bound.id),
        balance: None,
        error: None,
        old_balance_note: None,
        last_seen_at: Timestamp::UNIX_EPOCH,
        dismissed_at: None,
    };
    let connection = ConnectionView {
        id,
        provider: "simplefin".to_owned(),
        label: "Family bank".to_owned(),
        owner: MemberId::new(),
        status: ConnectionState::Active,
        last_error: None,
        last_attempt_at: None,
        last_success_at: None,
        next_sync_at: None,
        held_cursor: None,
        accounts: vec![row.clone()],
    };
    let views = HashMap::from([(bound.id, bound)]);
    let shared = Shared {
        accounts: &views,
        binding,
        timezone: &TimeZone::UTC,
        csrf: "csrf",
        can_manage: false,
        plaid_offered: false,
        forgettable: &HashSet::new(),
        refused_row: None,
    };
    belonging::account_item(&connection, &row, &shared, false).into_string()
}

fn token(scope: Scope) -> Actor {
    Actor {
        member: MemberId::new(),
        role: Role::Member,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope,
        },
    }
}

#[test]
fn read_access_is_offered_no_form() {
    let views = [view()];
    let actor = token(Scope::Read);
    let binding = Binding::new(&actor, &account_access_with_grants(&actor, &[]), &views);
    assert!(!binding.may_change(views[0].id));
    assert!(!binding.can_bind());
    assert_eq!(binding.offered_labels(&views).into_string(), "");
    let row = drawn(&binding, &views[0]);
    assert!(!row.contains("<form"));
    assert!(row.contains("Held for Child A"));
}

#[test]
fn a_write_grant_is_offered_the_form_on_what_it_covers() {
    let views = [view(), view()];
    let actor = token(Scope::Write);
    let grants = [TokenGrant {
        target: GrantTarget::Account(views[0].id),
        level: AccountLevel::Write,
    }];
    let binding = Binding::new(&actor, &account_access_with_grants(&actor, &grants), &views);
    assert!(binding.may_change(views[0].id));
    assert!(!binding.may_change(views[1].id));
    assert!(
        binding
            .offered_labels(&views)
            .into_string()
            .contains("Child A")
    );
    assert!(drawn(&binding, &views[0]).contains("/belonging"));
    assert!(!drawn(&binding, &views[1]).contains("<form"));
}

#[test]
fn a_lower_grant_is_offered_no_form() {
    let views = [view()];
    let actor = token(Scope::Write);
    let grants = [TokenGrant {
        target: GrantTarget::Account(views[0].id),
        level: AccountLevel::Transactions,
    }];
    let binding = Binding::new(&actor, &account_access_with_grants(&actor, &grants), &views);
    assert!(!drawn(&binding, &views[0]).contains("<form"));
    assert_eq!(binding.offered_labels(&views).into_string(), "");
}
