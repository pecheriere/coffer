//! Who may see a connection, forget one or release its cursor.

use super::{agent, session};
use crate::identity::{Role, Scope};
use crate::ids::{AccountId, MemberId};
use crate::policy::{
    AccountAccess, AccountLevel, Action, Denied, DenyReason, GrantTarget, TokenGrant,
    account_access_with_grants, authorize, connection_visible,
};

#[test]
fn a_connection_shows_through_an_account_it_serves_or_an_all_grant() {
    let holder = agent(Role::Member, Scope::Read);
    let (seen, unseen) = (AccountId::new(), AccountId::new());
    let granted =
        |target, level| account_access_with_grants(&holder, &[TokenGrant { target, level }]);
    let named = granted(GrantTarget::Account(seen), AccountLevel::Balances);
    assert!(connection_visible(&named, [seen]), "at any level");
    assert!(connection_visible(&named, [unseen, seen]), "one is enough");
    assert!(!connection_visible(&named, [unseen]));
    assert!(!connection_visible(&named, []), "unattached");
    let every = granted(GrantTarget::All, AccountLevel::Balances);
    assert!(connection_visible(&every, [unseen]));
    assert!(connection_visible(&every, []), "unattached");
    let uniform = AccountAccess::Uniform(AccountLevel::Balances);
    assert!(connection_visible(&uniform, []), "no grants hide nothing");
}

#[test]
fn forgetting_a_connection_needs_a_session_and_removals_role() {
    let member = session(Role::Member);
    let own = Action::ForgetConnection {
        owner: member.member,
    };
    let others = Action::ForgetConnection {
        owner: MemberId::new(),
    };
    assert_eq!(authorize(&member, own), Ok(()));
    assert_eq!(
        authorize(&member, others),
        Err(Denied {
            action: others,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    assert_eq!(authorize(&session(Role::Admin), others), Ok(()));
    // No token does, whatever its scope.
    let token = agent(Role::Admin, Scope::Admin);
    for action in [
        others,
        Action::ForgetConnection {
            owner: token.member,
        },
    ] {
        assert_eq!(
            authorize(&token, action),
            Err(Denied {
                action,
                reason: DenyReason::SessionRequired,
            })
        );
    }
}

#[test]
fn member_cannot_remove_others_connection_but_admin_can() {
    let member = session(Role::Member);
    let other = MemberId::new();
    let own = Action::RemoveConnection {
        owner: member.member,
    };
    let others = Action::RemoveConnection { owner: other };
    assert_eq!(authorize(&member, own), Ok(()));
    assert_eq!(
        authorize(&member, others),
        Err(Denied {
            action: others,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    assert_eq!(authorize(&session(Role::Admin), others), Ok(()));
    assert_eq!(authorize(&agent(Role::Admin, Scope::Admin), others), Ok(()));
    // An admin's write token is not admin.
    assert_eq!(
        authorize(&agent(Role::Admin, Scope::Write), others),
        Err(Denied {
            action: others,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    // A member's admin token is capped at write.
    assert_eq!(
        authorize(&agent(Role::Member, Scope::Admin), others),
        Err(Denied {
            action: others,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    // Even the owner needs write.
    let reader = agent(Role::Member, Scope::Read);
    let own_read = Action::RemoveConnection {
        owner: reader.member,
    };
    assert_eq!(
        authorize(&reader, own_read),
        Err(Denied {
            action: own_read,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Write
            },
        })
    );
}

#[test]
fn only_a_session_releases_a_held_cursor() {
    let member = session(Role::Member);
    let other = MemberId::new();
    let own = Action::ReleaseHeldCursor {
        owner: member.member,
    };
    let others = Action::ReleaseHeldCursor { owner: other };
    assert_eq!(authorize(&member, own), Ok(()));
    assert_eq!(
        authorize(&member, others),
        Err(Denied {
            action: others,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    assert_eq!(authorize(&session(Role::Admin), others), Ok(()));
    for token in [
        agent(Role::Admin, Scope::Admin),
        agent(Role::Member, Scope::Write),
    ] {
        for action in [
            own,
            others,
            Action::ReleaseHeldCursor {
                owner: token.member,
            },
        ] {
            assert_eq!(
                authorize(&token, action),
                Err(Denied {
                    action,
                    reason: DenyReason::SessionRequired,
                }),
                "a token never releases a cursor"
            );
        }
    }
}

#[test]
fn read_scope_cannot_manage_connections() {
    for action in [Action::ManageConnections, Action::WriteHousehold] {
        for actor in [
            agent(Role::Member, Scope::Read),
            agent(Role::Admin, Scope::Read),
        ] {
            assert_eq!(
                authorize(&actor, action),
                Err(Denied {
                    action,
                    reason: DenyReason::InsufficientScope {
                        needed: Scope::Write
                    },
                })
            );
        }
        for actor in [session(Role::Member), agent(Role::Member, Scope::Write)] {
            assert_eq!(authorize(&actor, action), Ok(()), "{action:?}");
        }
    }
}
