//! The rules page (spec §8.13): the rules that file or assign transactions nobody has filled
//! yet, in the order they run, and a form to add one. Each rule has a page of its own
//! ([`detail`]) showing what running it would change before it is run, with the forms that
//! run it, take back what it filled, change it and delete it. Every read and write goes
//! through the service the API uses, so who may do what and the audit stay in one place; the
//! page only leaves out the forms a viewer could not use, and the accounts it could not name.

mod change;
mod detail;
mod form;
mod runs;
mod words;

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, MemberId};
use coffer_domain::money::Currency;
use coffer_domain::policy::{Action, authorize};
use maud::{Markup, html};

use super::account::viewer;
use super::layout::{Frame, Nav, empty_state, page};
use super::{PageError, PageSession};
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::categories::CategoryGroupView;
use crate::ledger::rules::{RuleList, RuleView};
use crate::themes::Look;
use words::{amount_words, fills_words, match_words};

const PAGE: &str = "/rules";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PAGE, get(rules_page).post(form::create))
        .route("/rules/{id}", get(detail::rule_page).post(change::update))
        .route("/rules/{id}/apply", post(runs::apply))
        .route("/rules/{id}/ran", get(runs::ran))
        .route("/rules/{id}/undo", post(runs::undo))
        .route("/rules/{id}/taken-back", get(runs::taken_back))
        .route("/rules/{id}/delete", post(detail::delete))
}

fn refused(err: LedgerError) -> PageError {
    PageError(Problem::from(err))
}

/// An account a rule can name.
struct AccountName {
    name: String,
    currency: Currency,
    closed: bool,
    /// Whether the viewer may give a rule this account, as the ledger decides.
    writable: bool,
}

/// A member a rule can assign.
struct MemberName {
    name: String,
    disabled: bool,
}

/// What the ids a rule holds are called, and what its form offers: read through the services
/// the API uses, so a viewer sees only the accounts it may, and is offered only those the ledger
/// would let it give a rule.
struct Names {
    accounts: BTreeMap<AccountId, AccountName>,
    /// Whether the viewer may make a rule for every account, as the ledger decides.
    every_account: bool,
    groups: Vec<CategoryGroupView>,
    members: BTreeMap<MemberId, MemberName>,
}

/// Whether the ledger lets `actor` give a rule `account`, or every account for none. A refusal
/// is an answer; any other failure is the page's.
async fn may_give(
    state: &AppState,
    actor: &Actor,
    account: Option<AccountId>,
) -> Result<bool, PageError> {
    match state.ledger.authorize_rule_input(actor, account).await {
        Ok(()) => Ok(true),
        Err(LedgerError::Denied(_)) => Ok(false),
        Err(err) => Err(refused(err)),
    }
}

impl Names {
    async fn load(state: &AppState, actor: &Actor) -> Result<Self, PageError> {
        let mut accounts = BTreeMap::new();
        for account in state
            .ledger
            .list_accounts(actor, true)
            .await
            .map_err(refused)?
        {
            let name = AccountName {
                writable: may_give(state, actor, Some(account.id)).await?,
                name: account.name,
                currency: account.currency,
                closed: account.closed,
            };
            accounts.insert(account.id, name);
        }
        let every_account = may_give(state, actor, None).await?;
        let groups = state
            .ledger
            .list_categories(actor)
            .await
            .map_err(refused)?
            .items;
        let members = state
            .ledger
            .list_members(actor)
            .await
            .map_err(refused)?
            .into_iter()
            .map(|member| {
                let name = MemberName {
                    name: member.display_name.to_string(),
                    disabled: member.disabled_at.is_some(),
                };
                (member.id, name)
            })
            .collect();
        Ok(Self {
            accounts,
            every_account,
            groups,
            members,
        })
    }

    /// The account's name linking to its page, or plain words for one the viewer can't see.
    fn account_link(&self, id: AccountId) -> Markup {
        html! {
            @match self.accounts.get(&id) {
                Some(account) => a href={ "/accounts/" (id) } { (account.name) },
                None => "an account",
            }
        }
    }

    /// Where a rule applies: its account, linked, or every account.
    fn scope(&self, account: Option<AccountId>, every: &str) -> Markup {
        html! {
            @match account {
                Some(account) => (self.account_link(account)),
                None => (every),
            }
        }
    }

    fn category(&self, id: CategoryId) -> &str {
        self.groups
            .iter()
            .flat_map(|group| &group.categories)
            .find(|category| category.id == id)
            .map_or("a category", |category| category.name.as_str())
    }

    fn member(&self, id: MemberId) -> &str {
        self.members
            .get(&id)
            .map_or("a member", |member| member.name.as_str())
    }
}

async fn rules_page(
    State(state): State<AppState>,
    session: PageSession,
) -> Result<Response, PageError> {
    let actor = session.actor();
    let rules = state.ledger.list_rules(&actor).await.map_err(refused)?;
    let names = Names::load(&state, &actor).await?;
    let viewer = viewer(&state, &session).await;
    let look = session.look(&state).await;
    let frame = Frame {
        look,
        viewer: Some(&viewer),
        current: Some(Nav::More),
        narrow: false,
    };
    // None for a viewer who couldn't add one, who sees the list and no form.
    let csrf = authorize(&actor, Action::WriteHousehold)
        .is_ok()
        .then_some(viewer.csrf.as_str());
    Ok(page(&frame, "Rules", &content(look, &rules, &names, csrf)).into_response())
}

fn content(look: Look, rules: &RuleList, names: &Names, csrf: Option<&str>) -> Markup {
    html! {
        h1 { "Rules" }
        p {
            "A rule fills in what nobody has filled yet. When a transaction's bank description "
            "matches, it files it under a category, assigns it to someone, or both. Rules run in "
            "order on transactions as they arrive, and for each of the two the first rule that "
            "matches wins."
        }
        p {
            "A rule never marks anything reviewed and never changes what a person set. Open a "
            "rule to see what it would change in the transactions already here before you run it."
        }
        section.card id="rules" {
            @if rules.items.is_empty() {
                (empty_state(look, "No rules yet."))
            } @else {
                (rule_table(&rules.items, names))
            }
        }
        @if let Some(csrf) = csrf {
            section.card id="add-rule" {
                h2 { "Add a rule" }
                p {
                    "It runs on transactions arriving from now on, unless you turn that off. "
                    "Nothing already here changes until you run it from its page."
                }
                (form::fields(form::Target::Create, None, names, csrf))
            }
        }
    }
}

fn rule_table(rules: &[RuleView], names: &Names) -> Markup {
    html! {
        table.sums {
            thead {
                tr {
                    th scope="col" { "Order" }
                    th scope="col" { "When the description" }
                    th scope="col" { "On" }
                    th scope="col" { "It" }
                    th scope="col" { "New transactions" }
                }
            }
            tbody {
                @for (rank, rule) in (1_usize..).zip(rules) {
                    tr {
                        td { (rank) }
                        th scope="row" {
                            a href={ "/rules/" (rule.id) } { (match_words(rule.match_kind, &rule.pattern)) }
                        }
                        td {
                            (names.scope(rule.account, "Every account"))
                            @if let Some(amount) = rule.amount {
                                ", " (amount_words(amount))
                            }
                        }
                        td {
                            (fills_words(rule.category, rule.assignee, names))
                            @if rule.category_archived {
                                p.fine { "Its category is archived, so it files nothing for now." }
                            }
                            @if rule.assignee_disabled {
                                p.fine { "That member is disabled, so it assigns nobody for now." }
                            }
                        }
                        td { (if rule.enabled { "Runs" } else { "Off" }) }
                    }
                }
            }
        }
    }
}
