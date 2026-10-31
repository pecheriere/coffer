//! The authorization policy. The requirements table in [`authorize`] is the
//! only place access rules live.

use crate::identity::{Actor, Role, Scope, Via, role_ceiling};

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
}

/// Decides whether `actor` may perform `action`.
///
/// # Errors
/// Returns [`Denied`] with the reason when the actor lacks the requirement.
pub fn authorize(actor: &Actor, action: Action) -> Result<(), Denied> {
    let requirement = match action {
        Action::ReadHousehold => Requirement::Scope(Scope::Read),
        Action::ManageOwnPreferences => Requirement::Scope(Scope::Write),
        Action::ManageOwnCredentials => Requirement::Session,
        Action::ManageMembers | Action::ManageInvites | Action::ManageHousehold => {
            Requirement::Scope(Scope::Admin)
        }
        Action::IssueEnrollmentLink => Requirement::SessionWithScope(Scope::Admin),
    };
    let session = matches!(actor.via, Via::Session);
    let reason = match requirement {
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
    use crate::ids::{AgentTokenId, MemberId};

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
