//! Agent-token grants shared by the account and members pages: the level
//! selects, the edit form and the route saving it.

use std::collections::BTreeMap;
use std::str::FromStr as _;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Redirect;
use axum::routing::post;
use coffer_domain::ids::{AccountId, AgentTokenId};
use coffer_domain::policy::{
    AccountLevel, Action, GrantTarget, TokenGrant, authorize, grant_ceiling,
};
use coffer_store::AgentTokenRecord;
use jiff::Timestamp;
use maud::{Markup, html};
use serde::Deserialize;

use super::account::token_active;
use super::layout::csrf_field;
use super::{PageError, PageForm, PageSession};
use crate::http::AppState;
use crate::ledger::accounts::AccountView;

pub fn routes() -> Router<AppState> {
    Router::new().route("/account/tokens/{id}/grants", post(set_token_grants))
}

/// The form field and option of no grant.
pub(super) const NO_LEVEL: &str = "none";

/// What a grants form says above its selects.
pub(super) const LIMIT_HELP: &str = "Which accounts it may use. All accounts covers every account, including \
                          new ones, as far as the scope allows. To limit this token to chosen \
                          accounts, set All accounts to none, then choose their levels.";

/// The form field naming the page to return to after saving.
const BACK_FIELD: &str = "back";

/// What the edit form says to an admin, who edits a token that is not theirs.
const ADMIN_HELP: &str = "The member can widen this token again from their own account page. \
                          Every change is recorded in the audit log.";

/// The page a saved grants form returns to. It is named, never a URL, so a
/// forged form cannot redirect anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Back {
    Account,
    Members,
}

impl Back {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Members => "members",
        }
    }

    /// The form's value, or the account page for anything unknown.
    fn parse(value: &str) -> Self {
        if value == Self::Members.as_str() {
            Self::Members
        } else {
            Self::Account
        }
    }

    const fn path(self) -> &'static str {
        match self {
            Self::Account => "/account",
            Self::Members => "/admin/members",
        }
    }
}

/// The levels a form offers, lowest first.
pub(super) const LEVELS: [AccountLevel; 3] = [
    AccountLevel::Balances,
    AccountLevel::Transactions,
    AccountLevel::Write,
];

/// One level select: none, then the levels up to `ceiling`.
pub(super) fn level_field(
    label: &str,
    name: &str,
    selected: Option<AccountLevel>,
    ceiling: AccountLevel,
) -> Markup {
    html! {
        label.field {
            (label)
            select.input name=(name) {
                option value=(NO_LEVEL) selected[selected.is_none()] { (NO_LEVEL) }
                @for level in LEVELS.into_iter().filter(|level| *level <= ceiling) {
                    option value=(level.as_str()) selected[selected == Some(level)] {
                        (level.as_str())
                    }
                }
            }
        }
    }
}

pub(super) fn account_field(account: AccountId) -> String {
    format!("account_{account}")
}

/// A token's grants in words, naming accounts.
pub(super) fn grants_summary(grants: &[TokenGrant], accounts: &[AccountView]) -> String {
    if grants.is_empty() {
        return "All accounts, as far as its scope allows".to_owned();
    }
    grants
        .iter()
        .map(|grant| {
            let target = match grant.target {
                GrantTarget::All => "All accounts",
                GrantTarget::Account(id) => accounts
                    .iter()
                    .find(|account| account.id == id)
                    .map_or("An unknown account", |account| account.name.as_str()),
            };
            format!("{target}: {}", grant.level.as_str())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The form changing a working token's grants: its current levels, for every
/// open account and every closed one it holds a grant on (without a select,
/// saving would take that grant away). A token without grants starts at all
/// accounts as far as its scope allows, which is the access it has.
pub(super) fn grants_editor(
    token: &AgentTokenRecord,
    grants: &[TokenGrant],
    accounts: &[AccountView],
    now: Timestamp,
    csrf: &str,
    back: Back,
) -> Markup {
    if !token_active(token, now) {
        return html! {};
    }
    let ceiling = grant_ceiling(token.scope);
    let level_of = |target: GrantTarget| {
        grants
            .iter()
            .find(|grant| grant.target == target)
            .map(|grant| grant.level)
    };
    let all = if grants.is_empty() {
        Some(ceiling)
    } else {
        level_of(GrantTarget::All)
    };
    html! {
        details.item-detail {
            summary { "Change account access" }
            form method="post" action={ "/account/tokens/" (token.id) "/grants" } {
                (csrf_field(csrf))
                @if back == Back::Members {
                    input type="hidden" name=(BACK_FIELD) value=(Back::Members.as_str());
                }
                p.item-meta { (LIMIT_HELP) }
                @if back == Back::Members {
                    p.item-meta { (ADMIN_HELP) }
                }
                (level_field("All accounts", "all", all, ceiling))
                @for account in accounts {
                    @let level = level_of(GrantTarget::Account(account.id));
                    @if !account.closed || level.is_some() {
                        (level_field(&account.name, &account_field(account.id), level, ceiling))
                    }
                }
                button.btn type="submit" { "Save access" }
            }
        }
    }
}

/// What a form's level selects chose: fields `all` and `account_<id>`, each
/// `none` or a level. Anything else is refused whole and never echoed.
pub(super) struct GrantChoices {
    pub(super) all: Option<AccountLevel>,
    accounts: Vec<TokenGrant>,
}

impl GrantChoices {
    pub(super) fn parse(levels: &BTreeMap<String, String>) -> Result<Self, PageError> {
        let mut all = None;
        let mut accounts = Vec::new();
        for (field, value) in levels {
            let level = level_choice(value)?;
            if field == "all" {
                all = level;
                continue;
            }
            let account = field
                .strip_prefix("account_")
                .and_then(|id| AccountId::from_str(id).ok())
                .ok_or_else(|| PageError::invalid("the form could not be read"))?;
            if let Some(level) = level {
                accounts.push(TokenGrant {
                    target: GrantTarget::Account(account),
                    level,
                });
            }
        }
        Ok(Self { all, accounts })
    }

    /// The grants chosen. The best grant on an account wins, so an account
    /// set at or below All accounts would look restricted and not be: that
    /// is refused, whether All accounts was chosen or left at its default.
    pub(super) fn into_grants(self) -> Result<Vec<TokenGrant>, PageError> {
        if self
            .accounts
            .iter()
            .any(|grant| Some(grant.level) <= self.all)
        {
            return Err(PageError::invalid(
                "All accounts already gives this level to every account; to limit this token \
                 to chosen accounts, set All accounts to none",
            ));
        }
        let all = self.all.map(|level| TokenGrant {
            target: GrantTarget::All,
            level,
        });
        Ok(all.into_iter().chain(self.accounts).collect())
    }
}

fn level_choice(value: &str) -> Result<Option<AccountLevel>, PageError> {
    if value == NO_LEVEL {
        return Ok(None);
    }
    LEVELS
        .into_iter()
        .find(|level| level.as_str() == value)
        .map(Some)
        .ok_or_else(|| PageError::invalid("a level must be none, balances, transactions or write"))
}

#[derive(Deserialize)]
struct GrantsForm {
    #[serde(default)]
    csrf: String,
    /// Where to return, see [`Back`].
    #[serde(default)]
    back: String,
    /// The level selects, see [`GrantChoices`].
    #[serde(flatten)]
    levels: BTreeMap<String, String>,
}

/// Replaces a token's grants. Every level `none` is refused, by the service:
/// a token is widened only by an explicit All accounts level. The members
/// page is returned to only by someone who may use it.
async fn set_token_grants(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<GrantsForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = AgentTokenId::from_str(&id).map_err(|_| PageError::not_found())?;
    let grants = GrantChoices::parse(&form.levels)?.into_grants()?;
    state
        .agent_tokens
        .replace_grants(&actor, id, grants)
        .await?;
    let back = if authorize(&actor, Action::ManageMembers).is_ok() {
        Back::parse(&form.back)
    } else {
        Back::Account
    };
    Ok(Redirect::to(back.path()))
}
