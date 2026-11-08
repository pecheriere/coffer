//! The transactions pages: the list with its filters and the review queue, one transaction's
//! page, and the forms that review, file, split and edit one, mark several reviewed at once, and
//! enter or delete one by hand. Every read and write goes through the ledger service the API
//! uses, with the session's actor, so it authorizes and audits as for the API; the pages decide
//! nothing of their own, and a refusal is the service's fixed words.

mod detail;
mod edit;
mod filters;
mod list;
mod manual;
mod splits;

use axum::Router;
use axum::routing::{get, post};
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, CategoryId, MemberId, PlannedItemId, TransactionId};
use coffer_domain::money::Currency;
use coffer_domain::plan::PlanItemType;
use coffer_domain::policy::AccountLevel;
use coffer_domain::transactions::{ReviewStatus, TransactionFlag};
use maud::{Markup, html};
use std::collections::BTreeSet;

use super::PageError;
use crate::http::AppState;
use crate::http::problem::Problem;
use crate::ledger::LedgerError;
use crate::ledger::accounts::AccountView;
use crate::ledger::transactions::{AttributionView, TransactionView};

/// The list's path.
const LIST: &str = "/transactions";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(LIST, get(list::page).post(manual::create))
        .route("/transactions/batch", post(edit::batch))
        .route("/transactions/{id}", get(detail::page))
        .route("/transactions/{id}/review", post(edit::review))
        .route("/transactions/{id}/category", post(edit::category))
        .route("/transactions/{id}/splits", post(edit::splits))
        .route("/transactions/{id}/details", post(edit::details))
        .route("/transactions/{id}/delete", post(manual::delete))
}

/// A transaction's page.
fn detail_path(id: TransactionId) -> String {
    format!("{LIST}/{id}")
}

/// The service's refusal, in its own fixed words.
fn refused(err: LedgerError) -> PageError {
    PageError(Problem::from(err))
}

/// A transaction id from a path: anything else is a page that doesn't exist.
fn transaction_id(raw: &str) -> Result<TransactionId, PageError> {
    raw.parse().map_err(|_| PageError::not_found())
}

/// A category as the pages name and offer it.
struct CategoryName {
    id: CategoryId,
    /// `Group: Category`.
    label: String,
    /// Takes no new splits: offered only where a transaction is filed in it already.
    archived: bool,
}

impl CategoryName {
    /// Its label in a choice, saying when it is archived.
    fn choice_label(&self) -> Markup {
        html! {
            (self.label)
            @if self.archived { " (archived)" }
        }
    }
}

/// A goal a split may count towards.
struct GoalName {
    id: PlannedItemId,
    name: String,
    currency: Currency,
}

/// A member a transaction may be assigned to.
struct MemberName {
    id: MemberId,
    name: String,
    active: bool,
}

/// The names the pages show beside ids, read through the services the API uses, so each holds
/// only what the actor may see.
struct Names {
    accounts: Vec<AccountView>,
    /// The accounts whose transactions the actor may read.
    readable: BTreeSet<AccountId>,
    /// The accounts whose transactions the actor may change.
    writable: BTreeSet<AccountId>,
    categories: Vec<CategoryName>,
    goals: Vec<GoalName>,
    members: Vec<MemberName>,
}

impl Names {
    async fn read(state: &AppState, actor: &Actor) -> Result<Self, PageError> {
        let accounts = state
            .ledger
            .list_accounts(actor, true)
            .await
            .map_err(refused)?;
        let mut readable = BTreeSet::new();
        let mut writable = BTreeSet::new();
        for account in &accounts {
            if allowed(state, actor, account.id, AccountLevel::Transactions).await? {
                readable.insert(account.id);
                if allowed(state, actor, account.id, AccountLevel::Write).await? {
                    writable.insert(account.id);
                }
            }
        }
        let tree = state.ledger.list_categories(actor).await.map_err(refused)?;
        let categories = tree
            .items
            .into_iter()
            .flat_map(|group| {
                let (group_name, group_archived) = (group.name, group.archived);
                group
                    .categories
                    .into_iter()
                    .map(move |category| CategoryName {
                        id: category.id,
                        label: format!("{group_name}: {}", category.name),
                        archived: category.archived || group_archived,
                    })
            })
            .collect();
        let goals = state
            .ledger
            .list_planned_items(actor, true)
            .await
            .map_err(refused)?
            .into_iter()
            .filter(|item| item.schedule.item_type() == PlanItemType::Goal)
            .map(|item| GoalName {
                id: item.id,
                name: item.name,
                currency: item.amount.currency,
            })
            .collect();
        let members = state
            .ledger
            .list_members(actor)
            .await
            .map_err(refused)?
            .into_iter()
            .map(|member| MemberName {
                id: member.id,
                name: member.display_name.to_string(),
                active: member.disabled_at.is_none(),
            })
            .collect();
        Ok(Self {
            accounts,
            readable,
            writable,
            categories,
            goals,
            members,
        })
    }

    fn account(&self, id: AccountId) -> &str {
        self.accounts
            .iter()
            .find(|account| account.id == id)
            .map_or("an account", |account| account.name.as_str())
    }

    fn category(&self, id: CategoryId) -> &str {
        self.categories
            .iter()
            .find(|category| category.id == id)
            .map_or("a category", |category| category.label.as_str())
    }

    fn goal(&self, id: PlannedItemId) -> &str {
        self.goals
            .iter()
            .find(|goal| goal.id == id)
            .map_or("a goal", |goal| goal.name.as_str())
    }

    fn member(&self, id: MemberId) -> &str {
        self.members
            .iter()
            .find(|member| member.id == id)
            .map_or("a member", |member| member.name.as_str())
    }

    /// The accounts whose transactions the list may show, for its filter.
    fn transaction_accounts(&self) -> impl Iterator<Item = &AccountView> {
        self.accounts
            .iter()
            .filter(|account| self.readable.contains(&account.id))
    }

    /// Whether the service would take a change to a transaction in `account`.
    fn may_write(&self, account: AccountId) -> bool {
        self.writable.contains(&account)
    }

    /// The categories a split may name, in the tree's order, with `kept` even when archived: a
    /// part filed there already is sent back unchanged, which the service takes.
    fn offered_categories(&self, kept: Option<CategoryId>) -> impl Iterator<Item = &CategoryName> {
        self.categories
            .iter()
            .filter(move |category| !category.archived || Some(category.id) == kept)
    }

    /// Who made a change, in words: `Alice`, `Alice, through an agent`, `a rule`, `coffer`.
    fn by(&self, attribution: &AttributionView) -> String {
        match (attribution.member, attribution.rule) {
            (Some(member), _) if attribution.agent.is_some() => {
                format!("{}, through an agent", self.member(member))
            }
            (Some(member), None) => self.member(member).to_owned(),
            (Some(member), Some(_)) => format!("a rule {} ran", self.member(member)),
            (None, Some(_)) => "a rule".to_owned(),
            (None, None) => "coffer".to_owned(),
        }
    }
}

/// Whether the ledger lets `actor` use `account` at `level`: its refusal hides a form, any other
/// failure is the page's.
async fn allowed(
    state: &AppState,
    actor: &Actor,
    account: AccountId,
    level: AccountLevel,
) -> Result<bool, PageError> {
    match state
        .ledger
        .authorize_account_access(actor, account, level)
        .await
    {
        Ok(()) => Ok(true),
        Err(LedgerError::Denied(_)) => Ok(false),
        Err(err) => Err(refused(err)),
    }
}

/// What a transaction is filed as: its one category, how many parts, or nothing yet.
fn filed_as(transaction: &TransactionView, names: &Names) -> String {
    match transaction.splits.as_slice() {
        [] => "Uncategorized".to_owned(),
        [only] => names.category(only.category).to_owned(),
        several => format!("Split in {} parts", several.len()),
    }
}

/// The marks a transaction carries, each a chip.
fn chips(transaction: &TransactionView) -> Markup {
    html! {
        @if transaction.review == ReviewStatus::Unreviewed {
            " " span.chip { "To review" }
        }
        @if transaction.pending {
            " " span.chip.chip-quiet { "Pending" }
        }
        @for flag in &transaction.flags {
            " " span.chip { (flag_words(*flag)) }
        }
    }
}

fn flag_words(flag: TransactionFlag) -> &'static str {
    match flag {
        TransactionFlag::PossibleDuplicate => "Possible duplicate",
        TransactionFlag::SourceRemoved => "Taken back by the bank",
        TransactionFlag::SplitMismatch => "Splits don't add up",
    }
}

/// The name a transaction goes by: its payee, else its description.
fn title(transaction: &TransactionView) -> &str {
    transaction
        .payee
        .as_deref()
        .unwrap_or(&transaction.description)
}
