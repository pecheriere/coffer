use crate::identity::{Actor, Role, Scope, Via};
use crate::ids::{AccountId, AgentTokenId, MemberId};
use crate::policy::{
    AccountAccess, AccountLevel, AccountScope, Action, Denied, DenyReason, GrantTarget, TokenGrant,
    account_access_with_grants, authorize_account,
};

mod account_access;
mod connections;
mod grants;
mod household;

fn session(role: Role) -> Actor {
    Actor {
        member: MemberId::new(),
        role,
        via: Via::Session,
    }
}

fn agent(role: Role, scope: Scope) -> Actor {
    Actor {
        member: MemberId::new(),
        role,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope,
        },
    }
}

fn assert_account(
    actor: &Actor,
    access: &AccountAccess,
    account: AccountId,
    needed: AccountLevel,
    reason: Option<DenyReason>,
) {
    let expected = reason.map_or(Ok(()), |reason| {
        Err(Denied {
            action: Action::AccessAccount { needed },
            reason,
        })
    });
    assert_eq!(
        authorize_account(actor, access, account, needed),
        expected,
        "{actor:?} {needed:?}"
    );
}

/// What net worth sums, for each way of holding `balances` (spec §8.10 N2).
#[test]
fn net_worth_scope_is_what_balances_show() {
    let holder = agent(Role::Member, Scope::Read);
    let grant = |target, level| TokenGrant { target, level };
    let scope = |grants: &[TokenGrant]| {
        account_access_with_grants(&holder, grants).scope(AccountLevel::Balances)
    };

    assert_eq!(scope(&[]), AccountScope::All, "no grants is uniform");

    let (first, second) = (AccountId::new(), AccountId::new());
    let named = scope(&[
        grant(GrantTarget::Account(first), AccountLevel::Balances),
        grant(GrantTarget::Account(second), AccountLevel::Transactions),
    ]);
    let AccountScope::Only(mut seen) = named else {
        panic!("named grants are limited to those named");
    };
    let mut expected = vec![first, second];
    seen.sort();
    expected.sort();
    assert_eq!(seen, expected);

    assert_eq!(
        scope(&[grant(GrantTarget::All, AccountLevel::Balances)]),
        AccountScope::All
    );
}
