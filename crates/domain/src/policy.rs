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
//! | Add, reconnect or remove a connection    | `Write`                       |
//! | Create or change a category or group     | `Write`                       |
//!
//! Using an existing category on a split is a write to the transaction's
//! account, decided by [`authorize_account`]. A connection's last error is
//! shown only to an actor who sees every account at `Transactions`.

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

    /// The access as a report: uniform access shows `all` at the ceiling and
    /// no account; every level is the one [`Self::level_for`] gives.
    #[must_use]
    pub fn summary(&self) -> AccessSummary {
        match self {
            Self::Uniform(level) => AccessSummary {
                ceiling: *level,
                all: Some(*level),
                accounts: Vec::new(),
            },
            Self::Granted {
                ceiling,
                all,
                accounts,
            } => AccessSummary {
                ceiling: *ceiling,
                all: all.map(|all| all.min(*ceiling)),
                accounts: accounts
                    .keys()
                    .filter_map(|account| Some((*account, self.level_for(*account)?)))
                    .collect(),
            },
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

/// What an [`AccountAccess`] amounts to, for showing a token its own access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSummary {
    /// The most the actor's effective scope allows.
    pub ceiling: AccountLevel,
    /// The level on every account, including ones created later.
    pub all: Option<AccountLevel>,
    /// The level really held on each account a grant names, by account id.
    pub accounts: Vec<(AccountId, AccountLevel)>,
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
    /// Forget the connection `owner` added without giving its credentials up
    /// at the provider, which leaves the provider billing it. Never an agent
    /// token's: it needs a passkey session, and the same role as removal.
    ForgetConnection {
        owner: MemberId,
    },
    /// Let the sync cursor of the connection `owner` added move past records
    /// that keep failing to store. Those records are passed for good, so
    /// this needs a member signed in with a passkey: an agent token can read
    /// that a cursor is held but never give up the records.
    ReleaseHeldCursor {
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
        Action::ForgetConnection { owner } if owner == actor.member => {
            Requirement::SessionWithScope(Scope::Write)
        }
        Action::ReleaseHeldCursor { owner } if owner == actor.member => {
            Requirement::SessionWithScope(Scope::Write)
        }
        Action::AccessAccount { .. } => Requirement::Never(DenyReason::AccountHidden),
        Action::ManageOwnCredentials => Requirement::Session,
        Action::ManageMembers | Action::ManageInvites | Action::ManageHousehold => {
            Requirement::Scope(Scope::Admin)
        }
        Action::ForgetConnection { .. }
        | Action::ReleaseHeldCursor { .. }
        | Action::IssueEnrollmentLink => Requirement::SessionWithScope(Scope::Admin),
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
mod tests;
