use crate::identity::{Actor, Role, Scope, Via};
use crate::ids::{AccountId, AgentTokenId, MemberId};
use crate::policy::{AccountAccess, AccountLevel, Action, Denied, DenyReason, authorize_account};

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
