//! What a level on an account lets an actor do.

use super::{agent, assert_account, session};
use crate::identity::{Role, Scope};
use crate::ids::AccountId;
use crate::policy::{
    AccountAccess, AccountLevel, AccountScope, Action, Denied, DenyReason,
    account_access_for_scope, authorize,
};

#[test]
fn account_levels_are_ordered() {
    assert!(AccountLevel::Balances < AccountLevel::Transactions);
    assert!(AccountLevel::Transactions < AccountLevel::Write);
    assert_eq!(
        serde_json::to_string(&AccountLevel::Transactions).unwrap(),
        "\"transactions\""
    );
}

#[test]
fn read_token_reads_transactions_but_cannot_write() {
    let account = AccountId::new();
    for actor in [
        agent(Role::Member, Scope::Read),
        agent(Role::Admin, Scope::Read),
    ] {
        let access = account_access_for_scope(&actor);
        assert_eq!(access, AccountAccess::Uniform(AccountLevel::Transactions));
        assert_account(&actor, &access, account, AccountLevel::Balances, None);
        assert_account(&actor, &access, account, AccountLevel::Transactions, None);
        assert_account(
            &actor,
            &access,
            account,
            AccountLevel::Write,
            Some(DenyReason::AccountLevel {
                needed: AccountLevel::Write,
            }),
        );
        assert_eq!(access.scope(AccountLevel::Transactions), AccountScope::All);
        assert_eq!(
            access.scope(AccountLevel::Write),
            AccountScope::Only(Vec::new())
        );
    }
}

#[test]
fn write_token_writes_any_account() {
    for actor in [
        session(Role::Member),
        session(Role::Admin),
        agent(Role::Member, Scope::Write),
        agent(Role::Member, Scope::Admin),
        agent(Role::Admin, Scope::Write),
        agent(Role::Admin, Scope::Admin),
    ] {
        let access = account_access_for_scope(&actor);
        assert_eq!(access, AccountAccess::Uniform(AccountLevel::Write));
        for account in [AccountId::new(), AccountId::new()] {
            for needed in [
                AccountLevel::Balances,
                AccountLevel::Transactions,
                AccountLevel::Write,
            ] {
                assert_account(&actor, &access, account, needed, None);
                assert_eq!(access.level_for(account), Some(AccountLevel::Write));
            }
        }
        assert_eq!(access.scope(AccountLevel::Write), AccountScope::All);
    }
}

/// Access granted above what the actor's scope allows is capped: a read
/// token never writes, whatever access value it is handed.
#[test]
fn account_access_is_capped_by_the_actor_scope() {
    let actor = agent(Role::Admin, Scope::Read);
    let access = AccountAccess::Uniform(AccountLevel::Write);
    assert_account(
        &actor,
        &access,
        AccountId::new(),
        AccountLevel::Write,
        Some(DenyReason::AccountLevel {
            needed: AccountLevel::Write,
        }),
    );
}

#[test]
fn balances_access_denies_higher_levels() {
    let actor = session(Role::Member);
    let access = AccountAccess::Uniform(AccountLevel::Balances);
    let account = AccountId::new();
    assert_account(&actor, &access, account, AccountLevel::Balances, None);
    for needed in [AccountLevel::Transactions, AccountLevel::Write] {
        assert_account(
            &actor,
            &access,
            account,
            needed,
            Some(DenyReason::AccountLevel { needed }),
        );
    }
    assert_eq!(access.scope(AccountLevel::Balances), AccountScope::All);
    assert_eq!(
        access.scope(AccountLevel::Transactions),
        AccountScope::Only(Vec::new())
    );
}

/// `AccessAccount` is decided only by `authorize_account`; the household
/// table never grants it.
#[test]
fn household_authorize_never_grants_account_access() {
    let actor = session(Role::Admin);
    let action = Action::AccessAccount {
        needed: AccountLevel::Balances,
    };
    assert_eq!(
        authorize(&actor, action),
        Err(Denied {
            action,
            reason: DenyReason::AccountHidden,
        })
    );
}
