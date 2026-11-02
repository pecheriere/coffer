use super::*;
use crate::identity::{Actor, Role, Scope, Via};
use crate::ids::{AccountId, AgentTokenId, MemberId};
use proptest::strategy::Strategy;

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

#[test]
fn member_session_reads_but_cannot_manage_members() {
    let actor = session(Role::Member);
    assert_eq!(authorize(&actor, Action::ReadHousehold), Ok(()));
    assert_eq!(
        authorize(&actor, Action::ManageMembers),
        Err(Denied {
            action: Action::ManageMembers,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
}

#[test]
fn admin_with_write_token_cannot_manage_invites() {
    let actor = agent(Role::Admin, Scope::Write);
    assert_eq!(
        authorize(&actor, Action::ManageInvites),
        Err(Denied {
            action: Action::ManageInvites,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
}

#[test]
fn member_with_admin_token_is_capped() {
    let actor = agent(Role::Member, Scope::Admin);
    assert_eq!(actor.effective_scope(), Scope::Write);
    assert_eq!(
        authorize(&actor, Action::ManageMembers),
        Err(Denied {
            action: Action::ManageMembers,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
}

#[test]
fn agents_never_manage_credentials() {
    let actor = agent(Role::Admin, Scope::Admin);
    assert_eq!(
        authorize(&actor, Action::ManageOwnCredentials),
        Err(Denied {
            action: Action::ManageOwnCredentials,
            reason: DenyReason::SessionRequired,
        })
    );
    assert_eq!(
        authorize(&session(Role::Member), Action::ManageOwnCredentials),
        Ok(())
    );
}

#[test]
fn agents_cannot_issue_enrollment_links() {
    assert_eq!(
        authorize(
            &agent(Role::Admin, Scope::Admin),
            Action::IssueEnrollmentLink
        ),
        Err(Denied {
            action: Action::IssueEnrollmentLink,
            reason: DenyReason::SessionRequired,
        })
    );
    assert_eq!(
        authorize(&session(Role::Admin), Action::IssueEnrollmentLink),
        Ok(())
    );
    assert_eq!(
        authorize(&session(Role::Member), Action::IssueEnrollmentLink),
        Err(Denied {
            action: Action::IssueEnrollmentLink,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    // Agents keep the other admin actions.
    assert_eq!(
        authorize(&agent(Role::Admin, Scope::Admin), Action::ManageInvites),
        Ok(())
    );
}

#[test]
fn preferences_need_write_and_household_needs_admin() {
    let member = session(Role::Member);
    assert_eq!(authorize(&member, Action::ManageOwnPreferences), Ok(()));
    assert_eq!(
        authorize(&member, Action::ManageHousehold),
        Err(Denied {
            action: Action::ManageHousehold,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
    assert_eq!(
        authorize(&agent(Role::Admin, Scope::Write), Action::ManageHousehold),
        Err(Denied {
            action: Action::ManageHousehold,
            reason: DenyReason::InsufficientScope {
                needed: Scope::Admin
            },
        })
    );
}

/// Every role, credential and scope against the two appearance actions.
/// A token is capped by its member's role, so a member's admin token
/// still cannot change household settings.
#[test]
fn appearance_actions_matrix() {
    let table = [
        (session(Role::Member), true, false),
        (session(Role::Admin), true, true),
        (agent(Role::Member, Scope::Read), false, false),
        (agent(Role::Member, Scope::Write), true, false),
        (agent(Role::Member, Scope::Admin), true, false),
        (agent(Role::Admin, Scope::Read), false, false),
        (agent(Role::Admin, Scope::Write), true, false),
        (agent(Role::Admin, Scope::Admin), true, true),
    ];
    for (actor, preferences, household) in table {
        for (action, allowed, needed) in [
            (Action::ManageOwnPreferences, preferences, Scope::Write),
            (Action::ManageHousehold, household, Scope::Admin),
        ] {
            let expected = if allowed {
                Ok(())
            } else {
                Err(Denied {
                    action,
                    reason: DenyReason::InsufficientScope { needed },
                })
            };
            assert_eq!(authorize(&actor, action), expected, "{actor:?} {action:?}");
        }
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

#[test]
fn read_token_can_read() {
    let actor = agent(Role::Member, Scope::Read);
    assert_eq!(authorize(&actor, Action::ReadHousehold), Ok(()));
}

#[test]
fn token_scope_above_role_rejected() {
    assert_eq!(
        check_token_scope(Role::Member, Scope::Admin),
        Err(Denied {
            action: Action::ManageOwnCredentials,
            reason: DenyReason::ScopeAboveRole,
        })
    );
    assert_eq!(check_token_scope(Role::Member, Scope::Write), Ok(()));
    assert_eq!(check_token_scope(Role::Admin, Scope::Admin), Ok(()));
}

fn account(n: u128) -> AccountId {
    AccountId::from_uuid(uuid::Uuid::from_u128(n))
}

fn grant(target: GrantTarget, level: AccountLevel) -> TokenGrant {
    TokenGrant { target, level }
}

#[test]
fn uniform_without_grants() {
    for actor in [
        session(Role::Member),
        agent(Role::Member, Scope::Read),
        agent(Role::Admin, Scope::Write),
        agent(Role::Admin, Scope::Admin),
    ] {
        assert_eq!(
            account_access_with_grants(&actor, &[]),
            account_access_for_scope(&actor),
            "{actor:?}"
        );
    }
    // A session is never restricted by grants.
    let actor = session(Role::Member);
    let grants = [grant(
        GrantTarget::Account(account(1)),
        AccountLevel::Balances,
    )];
    assert_eq!(
        account_access_with_grants(&actor, &grants),
        AccountAccess::Uniform(AccountLevel::Write)
    );
}

#[test]
fn all_grant_plus_specific_takes_max() {
    let actor = agent(Role::Member, Scope::Write);
    let (x, y) = (account(1), account(2));
    let access = account_access_with_grants(
        &actor,
        &[
            grant(GrantTarget::All, AccountLevel::Balances),
            grant(GrantTarget::Account(x), AccountLevel::Write),
        ],
    );
    assert_eq!(access.level_for(x), Some(AccountLevel::Write));
    assert_eq!(access.level_for(y), Some(AccountLevel::Balances));
    assert_account(&actor, &access, x, AccountLevel::Write, None);
    assert_account(&actor, &access, y, AccountLevel::Balances, None);
    assert_account(
        &actor,
        &access,
        y,
        AccountLevel::Transactions,
        Some(DenyReason::AccountLevel {
            needed: AccountLevel::Transactions,
        }),
    );
    assert!(access.sees_every_account(AccountLevel::Balances));
    assert!(!access.sees_every_account(AccountLevel::Transactions));
}

/// A read token's grants never exceed `Transactions`, even when handed
/// grants that were never validated.
#[test]
fn ceiling_caps_grants() {
    let actor = agent(Role::Admin, Scope::Read);
    let x = account(1);
    let access = account_access_with_grants(
        &actor,
        &[
            grant(GrantTarget::All, AccountLevel::Write),
            grant(GrantTarget::Account(x), AccountLevel::Write),
        ],
    );
    for id in [x, account(2)] {
        assert_eq!(access.level_for(id), Some(AccountLevel::Transactions));
    }
    assert_eq!(access.scope(AccountLevel::Transactions), AccountScope::All);
    assert_eq!(
        access.scope(AccountLevel::Write),
        AccountScope::Only(Vec::new())
    );
    assert!(access.sees_every_account(AccountLevel::Transactions));
    assert!(!access.sees_every_account(AccountLevel::Write));
    assert_account(
        &actor,
        &access,
        x,
        AccountLevel::Write,
        Some(DenyReason::AccountLevel {
            needed: AccountLevel::Write,
        }),
    );
    // A member's admin token is capped by the role first.
    let member = agent(Role::Member, Scope::Admin);
    let access =
        account_access_with_grants(&member, &[grant(GrantTarget::All, AccountLevel::Write)]);
    assert_eq!(access.level_for(x), Some(AccountLevel::Write));
}

#[test]
fn no_grant_hides_account() {
    let actor = agent(Role::Admin, Scope::Admin);
    let (granted, hidden) = (account(1), account(2));
    let access = account_access_with_grants(
        &actor,
        &[grant(GrantTarget::Account(granted), AccountLevel::Write)],
    );
    assert_eq!(access.level_for(hidden), None);
    for needed in [
        AccountLevel::Balances,
        AccountLevel::Transactions,
        AccountLevel::Write,
    ] {
        assert_account(
            &actor,
            &access,
            hidden,
            needed,
            Some(DenyReason::AccountHidden),
        );
        assert!(!access.sees_every_account(needed));
    }
    assert_account(&actor, &access, granted, AccountLevel::Write, None);
}

#[test]
fn scope_lists_only_sufficient_accounts() {
    let actor = agent(Role::Admin, Scope::Write);
    let (balances, transactions, write) = (account(1), account(2), account(3));
    let access = account_access_with_grants(
        &actor,
        &[
            grant(GrantTarget::Account(write), AccountLevel::Write),
            grant(GrantTarget::Account(balances), AccountLevel::Balances),
            grant(
                GrantTarget::Account(transactions),
                AccountLevel::Transactions,
            ),
        ],
    );
    assert_eq!(
        access.scope(AccountLevel::Balances),
        AccountScope::Only(vec![balances, transactions, write])
    );
    assert_eq!(
        access.scope(AccountLevel::Transactions),
        AccountScope::Only(vec![transactions, write])
    );
    assert_eq!(
        access.scope(AccountLevel::Write),
        AccountScope::Only(vec![write])
    );
    // An `all` grant covers every account at its level, the specific
    // grants only above it.
    let access = account_access_with_grants(
        &actor,
        &[
            grant(GrantTarget::All, AccountLevel::Transactions),
            grant(GrantTarget::Account(write), AccountLevel::Write),
        ],
    );
    assert_eq!(access.scope(AccountLevel::Balances), AccountScope::All);
    assert_eq!(access.scope(AccountLevel::Transactions), AccountScope::All);
    assert_eq!(
        access.scope(AccountLevel::Write),
        AccountScope::Only(vec![write])
    );
}

#[test]
fn summary_reports_the_levels_really_held() {
    assert_eq!(
        AccountAccess::Uniform(AccountLevel::Transactions).summary(),
        AccessSummary {
            ceiling: AccountLevel::Transactions,
            all: Some(AccountLevel::Transactions),
            accounts: Vec::new(),
        }
    );
    let (x, y) = (account(1), account(2));
    let read = Actor {
        member: MemberId::new(),
        role: Role::Member,
        via: Via::Agent {
            token: AgentTokenId::new(),
            scope: Scope::Read,
        },
    };
    let summary = account_access_with_grants(
        &read,
        &[
            grant(GrantTarget::All, AccountLevel::Balances),
            grant(GrantTarget::Account(x), AccountLevel::Write),
            grant(GrantTarget::Account(y), AccountLevel::Balances),
        ],
    )
    .summary();
    assert_eq!(summary.ceiling, AccountLevel::Transactions);
    assert_eq!(summary.all, Some(AccountLevel::Balances));
    assert_eq!(
        summary.accounts.into_iter().collect::<BTreeMap<_, _>>(),
        BTreeMap::from([(x, AccountLevel::Transactions), (y, AccountLevel::Balances),]),
        "capped at the scope"
    );
}

#[test]
fn validate_grants_rules() {
    let x = account(1);
    assert_eq!(validate_grants(Scope::Write, &[]), Err(GrantError::Empty));
    let many: Vec<TokenGrant> = (0..=100)
        .map(|n| grant(GrantTarget::Account(account(n)), AccountLevel::Balances))
        .collect();
    assert_eq!(validate_grants(Scope::Write, &many[..100]), Ok(()));
    assert_eq!(
        validate_grants(Scope::Write, &many),
        Err(GrantError::TooMany)
    );
    for duplicate in [
        [
            grant(GrantTarget::Account(x), AccountLevel::Balances),
            grant(GrantTarget::Account(x), AccountLevel::Write),
        ],
        [
            grant(GrantTarget::All, AccountLevel::Balances),
            grant(GrantTarget::All, AccountLevel::Balances),
        ],
    ] {
        assert_eq!(
            validate_grants(Scope::Write, &duplicate),
            Err(GrantError::DuplicateTarget)
        );
    }
    let write = [
        grant(GrantTarget::All, AccountLevel::Balances),
        grant(GrantTarget::Account(x), AccountLevel::Write),
    ];
    assert_eq!(
        validate_grants(Scope::Read, &write),
        Err(GrantError::AboveScope)
    );
    assert_eq!(validate_grants(Scope::Write, &write), Ok(()));
    assert_eq!(validate_grants(Scope::Admin, &write), Ok(()));
    assert_eq!(
        validate_grants(
            Scope::Read,
            &[grant(GrantTarget::Account(x), AccountLevel::Transactions)]
        ),
        Ok(())
    );
}

#[test]
fn every_account_actions_need_an_all_grant() {
    let actor = agent(Role::Admin, Scope::Write);
    let denied = |needed| {
        Err(Denied {
            action: Action::AccessAccount { needed },
            reason: DenyReason::EveryAccount { needed },
        })
    };
    let only_one = account_access_with_grants(
        &actor,
        &[grant(GrantTarget::Account(account(1)), AccountLevel::Write)],
    );
    assert_eq!(
        authorize_every_account(&actor, &only_one, AccountLevel::Balances),
        denied(AccountLevel::Balances)
    );
    let all_transactions = account_access_with_grants(
        &actor,
        &[
            grant(GrantTarget::All, AccountLevel::Transactions),
            grant(GrantTarget::Account(account(1)), AccountLevel::Write),
        ],
    );
    assert_eq!(
        authorize_every_account(&actor, &all_transactions, AccountLevel::Transactions),
        Ok(())
    );
    assert_eq!(
        authorize_every_account(&actor, &all_transactions, AccountLevel::Write),
        denied(AccountLevel::Write)
    );
    // Uniform access passes at its level, capped by the actor's scope.
    let uniform = account_access_with_grants(&actor, &[]);
    assert_eq!(
        authorize_every_account(&actor, &uniform, AccountLevel::Write),
        Ok(())
    );
    let reader = agent(Role::Admin, Scope::Read);
    assert_eq!(
        authorize_every_account(
            &reader,
            &AccountAccess::Uniform(AccountLevel::Write),
            AccountLevel::Write
        ),
        denied(AccountLevel::Write)
    );
}

#[test]
fn grant_ceiling_follows_scope() {
    assert_eq!(grant_ceiling(Scope::Read), AccountLevel::Transactions);
    assert_eq!(grant_ceiling(Scope::Write), AccountLevel::Write);
    assert_eq!(grant_ceiling(Scope::Admin), AccountLevel::Write);
}

#[test]
fn grant_target_serializes_as_all_or_the_account_id() {
    let x = account(1);
    let grants = [
        grant(GrantTarget::All, AccountLevel::Balances),
        grant(GrantTarget::Account(x), AccountLevel::Write),
    ];
    let json = serde_json::to_string(&grants).unwrap();
    assert_eq!(
        json,
        format!(r#"[{{"target":"all","level":"balances"}},{{"target":"{x}","level":"write"}}]"#)
    );
    assert_eq!(
        serde_json::from_str::<Vec<TokenGrant>>(&json).unwrap(),
        grants
    );
    assert!(
        serde_json::from_str::<TokenGrant>(r#"{"target":"all","level":"write","x":1}"#).is_err()
    );
    for bad in ["\"All\"", "\"\"", "\"not-an-id\"", "null"] {
        assert!(serde_json::from_str::<GrantTarget>(bad).is_err(), "{bad}");
    }
    // Only the canonical lowercase form, like every id.
    let upper = format!("\"{}\"", account(0xabcd).to_string().to_uppercase());
    assert!(serde_json::from_str::<GrantTarget>(&upper).is_err());
}

const LEVELS: [AccountLevel; 3] = [
    AccountLevel::Balances,
    AccountLevel::Transactions,
    AccountLevel::Write,
];

fn level() -> impl Strategy<Value = AccountLevel> {
    proptest::sample::select(LEVELS.to_vec())
}

fn target() -> impl Strategy<Value = GrantTarget> {
    proptest::option::of(0..6_u128)
        .prop_map(|n| n.map_or(GrantTarget::All, |n| GrantTarget::Account(account(n))))
}

proptest::proptest! {
    /// Grants are random and unvalidated (above the scope, repeated
    /// targets): the level is still never above the scope ceiling or
    /// the best grant naming the account, an account no grant names is
    /// hidden, and `scope` and `sees_every_account` agree with
    /// `authorize_account`.
    #[test]
    fn level_for_never_exceeds_ceiling_or_grant(
        role in proptest::sample::select(vec![Role::Member, Role::Admin]),
        scope in proptest::sample::select(vec![Scope::Read, Scope::Write, Scope::Admin]),
        grants in proptest::collection::vec(
            (target(), level()).prop_map(|(target, level)| TokenGrant { target, level }),
            1..8,
        ),
    ) {
        let actor = agent(role, scope);
        let access = account_access_with_grants(&actor, &grants);
        let ceiling = grant_ceiling(actor.effective_scope());
        // Account 6 is never named by a grant.
        for id in (0..=6).map(account) {
            let best = grants
                .iter()
                .filter(|g| g.target == GrantTarget::All || g.target == GrantTarget::Account(id))
                .map(|g| g.level)
                .max();
            let level = access.level_for(id);
            proptest::prop_assert_eq!(level.is_some(), best.is_some());
            if let (Some(level), Some(best)) = (level, best) {
                proptest::prop_assert_eq!(level, best.min(ceiling));
            }
            for needed in LEVELS {
                let allowed = authorize_account(&actor, &access, id, needed).is_ok();
                let listed = match access.scope(needed) {
                    AccountScope::All => true,
                    AccountScope::Only(ids) => ids.contains(&id),
                };
                proptest::prop_assert_eq!(listed, allowed, "{:?} {:?}", id, needed);
                if access.sees_every_account(needed) {
                    proptest::prop_assert!(allowed);
                }
                proptest::prop_assert_eq!(
                    authorize_every_account(&actor, &access, needed).is_ok(),
                    access.sees_every_account(needed)
                );
            }
        }
    }
}
