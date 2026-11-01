//! The authorization policy. The requirements table in [`authorize`] holds
//! the household rules; [`authorize_account`] decides everything tied to one
//! account. No access rule lives anywhere else.
//!
//! An agent token may carry [`TokenGrant`]s limiting it to chosen accounts at
//! chosen levels. Its effective level on an account is the best grant naming
//! it (or `all`), capped by [`grant_ceiling`] of its effective scope; an
//! account no grant names is hidden. A token without grants, and every
//! session, has uniform access at its scope.
//!
//! Actions that touch more than one account, or accounts not yet granted,
//! need an `all` grant ([`AccountAccess::sees_every_account`], checked by
//! [`authorize_every_account`]):
//!
//! | Action                                   | Needs `sees_every_account` at |
//! |------------------------------------------|-------------------------------|
//! | Read the audit log                       | `Transactions`                |
//! | List unbound provider accounts           | `Transactions`                |
//! | Read a connection's sync runs            | `Transactions`                |
//! | Create an account                        | `Write`                       |
//! | Bind a provider account                  | `Write`                       |
//! | Trigger a sync                           | `Write`                       |
//! | Reconnect or remove a connection         | `Write`                       |

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::identity::{Actor, Role, Scope, Via, role_ceiling};
use crate::ids::{AccountId, MemberId, ParseIdError};

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
    /// An agent token's grants. Built only by [`account_access_with_grants`];
    /// every level read from it is capped at `ceiling`.
    #[non_exhaustive]
    Granted {
        /// The highest level the token's effective scope allows.
        ceiling: AccountLevel,
        /// The level on every account, from an `all` grant.
        all: Option<AccountLevel>,
        /// The levels granted on single accounts.
        accounts: BTreeMap<AccountId, AccountLevel>,
    },
}

impl AccountAccess {
    /// The level granted on `account`, or `None` when it is hidden.
    #[must_use]
    pub fn level_for(&self, account: AccountId) -> Option<AccountLevel> {
        match self {
            Self::Uniform(level) => Some(*level),
            Self::Granted {
                ceiling,
                all,
                accounts,
            } => (*all)
                .max(accounts.get(&account).copied())
                .map(|level| level.min(*ceiling)),
        }
    }

    /// The accounts granted at least `at_least`, for filtering queries.
    #[must_use]
    pub fn scope(&self, at_least: AccountLevel) -> AccountScope {
        match self {
            Self::Uniform(level) if *level >= at_least => AccountScope::All,
            Self::Uniform(_) => AccountScope::Only(Vec::new()),
            Self::Granted { .. } if self.sees_every_account(at_least) => AccountScope::All,
            Self::Granted { accounts, .. } => AccountScope::Only(
                accounts
                    .keys()
                    .copied()
                    .filter(|account| self.level_for(*account) >= Some(at_least))
                    .collect(),
            ),
        }
    }

    /// Whether every account, including ones created later, is granted at
    /// least `at_least`. Needed by the actions in the module's rule table.
    #[must_use]
    pub fn sees_every_account(&self, at_least: AccountLevel) -> bool {
        match self {
            Self::Uniform(level) => *level >= at_least,
            Self::Granted { ceiling, all, .. } => {
                all.is_some_and(|all| all.min(*ceiling) >= at_least)
            }
        }
    }
}

/// What a grant covers: every account, or one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub enum GrantTarget {
    /// Every account, including ones created after the grant. Written `all`.
    All,
    /// One account. Written as its id.
    Account(AccountId),
}

impl fmt::Display for GrantTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => f.write_str("all"),
            Self::Account(account) => fmt::Display::fmt(account, f),
        }
    }
}

impl FromStr for GrantTarget {
    type Err = ParseIdError;

    /// Accepts `all` or an account id in canonical form.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            Ok(Self::All)
        } else {
            s.parse().map(Self::Account)
        }
    }
}

impl TryFrom<String> for GrantTarget {
    type Error = ParseIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl Serialize for GrantTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// One grant of an agent token: `level` on `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenGrant {
    /// The accounts the grant covers.
    pub target: GrantTarget,
    /// The level granted on them.
    pub level: AccountLevel,
}

/// The most grants one token may carry.
pub const MAX_GRANTS: usize = 100;

/// Why a token's grants were rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GrantError {
    /// No grants were given.
    #[error("a token with grants needs at least one")]
    Empty,
    /// More than [`MAX_GRANTS`] grants were given.
    #[error("a token has at most {MAX_GRANTS} grants")]
    TooMany,
    /// A target was granted twice.
    #[error("each account, and all, is granted at most once")]
    DuplicateTarget,
    /// A level is above [`grant_ceiling`] of the token's scope.
    #[error("a grant is above what the token's scope allows")]
    AboveScope,
}

/// Checks the grants of a token with `scope`: 1 to [`MAX_GRANTS`] of them,
/// no target twice, none above [`grant_ceiling`].
///
/// # Errors
/// Returns the first [`GrantError`] found.
pub fn validate_grants(scope: Scope, grants: &[TokenGrant]) -> Result<(), GrantError> {
    if grants.is_empty() {
        return Err(GrantError::Empty);
    }
    if grants.len() > MAX_GRANTS {
        return Err(GrantError::TooMany);
    }
    let ceiling = grant_ceiling(scope);
    let mut targets = BTreeSet::new();
    for grant in grants {
        if !targets.insert(grant.target) {
            return Err(GrantError::DuplicateTarget);
        }
        if grant.level > ceiling {
            return Err(GrantError::AboveScope);
        }
    }
    Ok(())
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
#[must_use]
pub fn grant_ceiling(scope: Scope) -> AccountLevel {
    match scope {
        Scope::Read => AccountLevel::Transactions,
        Scope::Write | Scope::Admin => AccountLevel::Write,
    }
}

/// Uniform access at the level the actor's effective scope allows: every
/// session, and every agent token without grants.
#[must_use]
pub fn account_access_for_scope(actor: &Actor) -> AccountAccess {
    AccountAccess::Uniform(grant_ceiling(actor.effective_scope()))
}

/// The access of `actor` holding `grants`: uniform at its scope for a
/// session or when `grants` is empty, otherwise limited to the grants and
/// capped at its effective scope. A target granted twice keeps its best
/// level, still capped.
#[must_use]
pub fn account_access_with_grants(actor: &Actor, grants: &[TokenGrant]) -> AccountAccess {
    if grants.is_empty() || matches!(actor.via, Via::Session) {
        return account_access_for_scope(actor);
    }
    let ceiling = grant_ceiling(actor.effective_scope());
    let mut all = None;
    let mut accounts = BTreeMap::new();
    for grant in grants {
        let level = grant.level.min(ceiling);
        match grant.target {
            GrantTarget::All => all = all.max(Some(level)),
            GrantTarget::Account(account) => {
                accounts
                    .entry(account)
                    .and_modify(|held: &mut AccountLevel| *held = (*held).max(level))
                    .or_insert(level);
            }
        }
    }
    AccountAccess::Granted {
        ceiling,
        all,
        accounts,
    }
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
    /// The action needs `needed` on every account, including ones created
    /// later, and the actor's grants do not give it.
    EveryAccount {
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
        Some(level) if level.min(grant_ceiling(actor.effective_scope())) >= needed => {
            return Ok(());
        }
        Some(_) => DenyReason::AccountLevel { needed },
    };
    Err(Denied {
        action: Action::AccessAccount { needed },
        reason,
    })
}

/// Decides whether `actor`, holding `access`, may perform an action that
/// needs `needed` on every account (the module's rule table). Capped by the
/// actor's effective scope like [`authorize_account`].
///
/// # Errors
/// Returns [`Denied`] with action [`Action::AccessAccount`] and reason
/// [`DenyReason::EveryAccount`] unless every account is granted at least
/// `needed`.
pub fn authorize_every_account(
    actor: &Actor,
    access: &AccountAccess,
    needed: AccountLevel,
) -> Result<(), Denied> {
    if access.sees_every_account(needed) && grant_ceiling(actor.effective_scope()) >= needed {
        Ok(())
    } else {
        Err(Denied {
            action: Action::AccessAccount { needed },
            reason: DenyReason::EveryAccount { needed },
        })
    }
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
            format!(
                r#"[{{"target":"all","level":"balances"}},{{"target":"{x}","level":"write"}}]"#
            )
        );
        assert_eq!(
            serde_json::from_str::<Vec<TokenGrant>>(&json).unwrap(),
            grants
        );
        assert!(
            serde_json::from_str::<TokenGrant>(r#"{"target":"all","level":"write","x":1}"#)
                .is_err()
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
}
