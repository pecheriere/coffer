//! The authorization policy. The requirements table in [`authorize`] holds
//! the household rules; [`authorize_account`] decides everything tied to one
//! account. No access rule lives anywhere else.

use serde::{Deserialize, Serialize};

use crate::identity::{Actor, Role, Scope, Via, role_ceiling};
use crate::ids::{AccountId, MemberId};

/// How much of one account an actor may use. Ordered
/// `Balances < Transactions < Write`; each level includes those below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AccountLevel {
    /// The account and its balances.
    Balances,
    /// Also its transactions and source records.
    Transactions,
    /// Also every change to them.
    Write,
}

impl AccountLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balances => "balances",
            Self::Transactions => "transactions",
            Self::Write => "write",
        }
    }
}

/// The accounts an actor may use and at which level. Obtained only from the
/// server's access resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountAccess {
    /// The same level on every account.
    Uniform(AccountLevel),
}

impl AccountAccess {
    /// The level granted on `account`, or `None` when it is hidden.
    #[must_use]
    pub fn level_for(&self, _account: AccountId) -> Option<AccountLevel> {
        match self {
            Self::Uniform(level) => Some(*level),
        }
    }

    /// The accounts granted at least `at_least`, for filtering queries.
    #[must_use]
    pub fn scope(&self, at_least: AccountLevel) -> AccountScope {
        match self {
            Self::Uniform(level) if *level >= at_least => AccountScope::All,
            Self::Uniform(_) => AccountScope::Only(Vec::new()),
        }
    }
}

/// The accounts an account-scoped query may return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountScope {
    All,
    /// Only these accounts; empty means none.
    Only(Vec<AccountId>),
}

/// The highest account level an effective scope allows: a read scope never
/// writes.
fn level_ceiling(scope: Scope) -> AccountLevel {
    match scope {
        Scope::Read => AccountLevel::Transactions,
        Scope::Write | Scope::Admin => AccountLevel::Write,
    }
}

/// The access every actor has until per-token grants exist: uniform, at the
/// level its effective scope allows.
#[must_use]
pub fn account_access_for_scope(actor: &Actor) -> AccountAccess {
    AccountAccess::Uniform(level_ceiling(actor.effective_scope()))
}

/// Something an actor may attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ReadHousehold,
    ManageOwnCredentials,
    ManageMembers,
    ManageInvites,
    /// Issue a new-member or re-enroll link. Redeeming one registers a
    /// passkey, so an agent able to issue links could give itself a session;
    /// this needs an admin signed in with a passkey.
    IssueEnrollmentLink,
    /// Change the acting member's own theme and colour scheme. Only ever
    /// the actor's own member: there is no target to name.
    ManageOwnPreferences,
    /// Change household-wide settings, such as the household theme.
    ManageHousehold,
    /// Use one account at `needed`. Decided only by [`authorize_account`]:
    /// [`authorize`] never grants it.
    AccessAccount {
        needed: AccountLevel,
    },
    /// Change household-wide data, such as categories.
    WriteHousehold,
    /// Add or reconnect a connection, trigger a sync, bind provider
    /// accounts.
    ManageConnections,
    /// Remove or reconnect the connection `owner` added.
    RemoveConnection {
        owner: MemberId,
    },
}

/// Why an action was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    SessionRequired,
    InsufficientScope {
        needed: Scope,
    },
    /// A token scope above what the member's role may exercise was asked
    /// for.
    ScopeAboveRole,
    /// The account is not visible to the actor. Answered exactly like a
    /// missing account.
    AccountHidden,
    /// The account is visible, but not at the level needed.
    AccountLevel {
        needed: AccountLevel,
    },
}

/// An authorization failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denied {
    pub action: Action,
    pub reason: DenyReason,
}

enum Requirement {
    Session,
    Scope(Scope),
    SessionWithScope(Scope),
    /// Never granted here.
    Never(DenyReason),
}

/// Decides whether `actor` may perform `action`.
///
/// # Errors
/// Returns [`Denied`] with the reason when the actor lacks the requirement.
pub fn authorize(actor: &Actor, action: Action) -> Result<(), Denied> {
    let requirement = match action {
        Action::ReadHousehold => Requirement::Scope(Scope::Read),
        Action::ManageOwnPreferences | Action::WriteHousehold | Action::ManageConnections => {
            Requirement::Scope(Scope::Write)
        }
        Action::RemoveConnection { owner } if owner == actor.member => {
            Requirement::Scope(Scope::Write)
        }
        Action::RemoveConnection { .. } => Requirement::Scope(Scope::Admin),
        Action::AccessAccount { .. } => Requirement::Never(DenyReason::AccountHidden),
        Action::ManageOwnCredentials => Requirement::Session,
        Action::ManageMembers | Action::ManageInvites | Action::ManageHousehold => {
            Requirement::Scope(Scope::Admin)
        }
        Action::IssueEnrollmentLink => Requirement::SessionWithScope(Scope::Admin),
    };
    let session = matches!(actor.via, Via::Session);
    let reason = match requirement {
        Requirement::Never(reason) => reason,
        Requirement::Session | Requirement::SessionWithScope(_) if !session => {
            DenyReason::SessionRequired
        }
        Requirement::Session => return Ok(()),
        Requirement::Scope(needed) | Requirement::SessionWithScope(needed) => {
            if actor.effective_scope() >= needed {
                return Ok(());
            }
            DenyReason::InsufficientScope { needed }
        }
    };
    Err(Denied { action, reason })
}

/// Decides whether `actor`, holding `access`, may use `account` at
/// `needed`. The level granted is capped by the actor's effective scope, so
/// a read-scoped actor never writes whatever `access` says.
///
/// # Errors
/// Returns [`Denied`] with action [`Action::AccessAccount`] and reason
/// [`DenyReason::AccountHidden`] when the account is not visible, or
/// [`DenyReason::AccountLevel`] when its level is below `needed`.
pub fn authorize_account(
    actor: &Actor,
    access: &AccountAccess,
    account: AccountId,
    needed: AccountLevel,
) -> Result<(), Denied> {
    let reason = match access.level_for(account) {
        None => DenyReason::AccountHidden,
        Some(level) if level.min(level_ceiling(actor.effective_scope())) >= needed => {
            return Ok(());
        }
        Some(_) => DenyReason::AccountLevel { needed },
    };
    Err(Denied {
        action: Action::AccessAccount { needed },
        reason,
    })
}

/// Rejects a token scope above what `role` may exercise.
///
/// # Errors
/// Returns [`Denied`] (with `action` [`Action::ManageOwnCredentials`] and
/// reason [`DenyReason::ScopeAboveRole`]) when `requested` exceeds the role
/// ceiling.
pub fn check_token_scope(role: Role, requested: Scope) -> Result<(), Denied> {
    let ceiling = role_ceiling(role);
    if requested <= ceiling {
        Ok(())
    } else {
        Err(Denied {
            action: Action::ManageOwnCredentials,
            reason: DenyReason::ScopeAboveRole,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Actor, Role, Scope, Via};
    use crate::ids::{AccountId, AgentTokenId, MemberId};

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
}
