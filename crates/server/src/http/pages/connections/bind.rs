//! Binding a provider account from the page: create the account it feeds,
//! or link it to an open account in its currency. The forms ask the sync
//! service, which decides; the page only leaves out forms that the
//! actor's role and access could not use.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use coffer_domain::accounts::{AccountKind, BelongsTo, default_belongs_to};
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, Action, authorize, authorize_account, authorize_every_account,
};
use maud::{Markup, html};
use serde::Deserialize;

use super::belonging::{
    BELONGS_TO_UNKNOWN, SEEN_BY, answer_options, beneficiaries, held_for_field, in_form_words,
    kind_field, parse_belongs_to,
};
use super::{Notice, back, refused_on_row, render};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::{AccountView, assert_no_debug};
use crate::sync::connections::{
    ConnectionState, ConnectionView, NewLinkedAccountInput, ProviderAccountView,
};

pub(super) const KIND_UNKNOWN: &str = "Choose one of the account kinds in the list.";
const ACCOUNT_UNKNOWN: &str = "Choose the account to link to.";
/// Under a kind preselected from the provider's name, which is only a guess.
const SUGGESTED: &str =
    "Suggested from the account's name, so check it: the kind decides what counts as cash.";
/// The same, under a kind that by itself leaves the account out of net worth.
const SUGGESTED_LEAVING_OUT: &str = "Suggested from the account's name, so check it: the kind \
    decides what counts as cash, and this kind leaves the account out of net worth unless you say \
    it's the household's.";
/// The longest account name, in characters; the form's `maxlength`.
const NAME_CHARS: usize = 100;

/// What the actor could do with the binding forms, and the open accounts a
/// provider account could be linked to.
pub(super) struct Binding {
    may_create: bool,
    may_link: bool,
    open: Vec<OpenAccount>,
    /// The accounts the actor may change, closed ones too.
    writable: HashSet<AccountId>,
}

struct OpenAccount {
    id: AccountId,
    name: String,
    currency: &'static str,
}

impl Binding {
    /// The forms `actor` can use, given the accounts they see.
    pub(super) async fn load(
        state: &AppState,
        actor: &Actor,
        accounts: &[AccountView],
    ) -> Result<Self, PageError> {
        let access = state.access.resolve(actor).await?;
        Ok(Self::new(actor, &access, accounts))
    }

    /// The forms `actor`, holding `access`, can use.
    fn new(actor: &Actor, access: &AccountAccess, accounts: &[AccountView]) -> Self {
        let may_link = authorize(actor, Action::ManageConnections).is_ok()
            && authorize_every_account(actor, access, AccountLevel::Write).is_ok();
        let may_create = may_link && authorize(actor, Action::WriteHousehold).is_ok();
        let writable: HashSet<AccountId> = accounts
            .iter()
            .map(|account| account.id)
            .filter(|id| authorize_account(actor, access, *id, AccountLevel::Write).is_ok())
            .collect();
        let open = accounts
            .iter()
            .filter(|account| !account.closed && writable.contains(&account.id))
            .map(|account| OpenAccount {
                id: account.id,
                name: account.name.clone(),
                currency: account.currency.as_str(),
            })
            .collect();
        Self {
            may_create,
            may_link,
            open,
            writable,
        }
    }

    /// Whether the actor could create an account for a provider account.
    pub(super) fn can_bind(&self) -> bool {
        self.may_create
    }

    /// Whether the actor may change the account `id`.
    pub(super) fn may_change(&self, id: AccountId) -> bool {
        self.writable.contains(&id)
    }

    /// The labels the forms' `Held for` offers: nothing for an actor who has no such form.
    pub(super) fn offered_labels(&self, accounts: &[AccountView]) -> Markup {
        if self.may_create || !self.writable.is_empty() {
            beneficiaries(accounts)
        } else {
            html! {}
        }
    }

    /// The forms for an unbound provider account of a live connection,
    /// unfolded when `unfolded`; nothing for one that is bound or
    /// whose connection is removed. When `refused` these answer a refusal of them: the member
    /// may have rejected the guess, so it isn't put back, and they stay open to be seen.
    pub(super) fn forms(
        &self,
        connection: &ConnectionView,
        account: &ProviderAccountView,
        csrf: &str,
        unfolded: bool,
        refused: bool,
    ) -> Markup {
        if account.account.is_some()
            || connection.status == ConnectionState::Removed
            || !self.may_create
        {
            return html! {};
        }
        // Two provider accounts of one connection may not feed one account.
        let candidates: Vec<&OpenAccount> = if self.may_link {
            self.open
                .iter()
                .filter(|open| open.currency.eq_ignore_ascii_case(&account.currency))
                .filter(|open| {
                    !connection
                        .accounts
                        .iter()
                        .any(|fed| fed.account == Some(open.id))
                })
                .collect()
        } else {
            Vec::new()
        };
        let id = account.id;
        let name: String = account.name.chars().take(NAME_CHARS).collect();
        // Submitting is the member's confirmation, of a value they can see: what the form posts is
        // the only kind that a guess ever reaches the service as.
        let guess = account.suggested_kind.filter(|_| !refused);
        let preselected = account.kind_hint.or(guess);
        html! {
            details.item-detail open[unfolded || refused] {
                summary { "Bind to an account" }
                form method="post" action={ "/connections/provider-accounts/" (id) "/create-account" } {
                    (csrf_field(csrf))
                    label.field {
                        "New account name"
                        input .input type="text" name="name" value=(name)
                            maxlength=(NAME_CHARS) required;
                    }
                    (kind_field(id, &html! {
                        @if preselected.is_none() {
                            option value="" selected disabled { "Choose a kind" }
                        }
                        @for kind in AccountKind::ALL {
                            option value=(kind.as_str()) selected[preselected == Some(kind)] {
                                (kind_label(kind))
                            }
                        }
                    }, guess.map(guess_sentence)))
                    label.field {
                        "Whose money is it?"
                        select .input name="belongs_to" {
                            option value="" selected { "Follow the kind" }
                            (answer_options())
                        }
                    }
                    (held_for_field(id, None, &format!("{SEEN_BY} Leave it empty if this is the household's money.")))
                    button.btn.btn-primary type="submit" { "Create account and bind" }
                }
                @if !candidates.is_empty() {
                    form method="post" action={ "/connections/provider-accounts/" (id) "/bind" } {
                        (csrf_field(csrf))
                        label.field {
                            "Or link to an existing account"
                            select .input name="account" required {
                                @for open in &candidates {
                                    option value=(open.id) { (open.name) }
                                }
                            }
                        }
                        button.btn type="submit" { "Link account" }
                    }
                }
            }
        }
    }
}

/// What a form says under the guess `kind`.
fn guess_sentence(kind: AccountKind) -> &'static str {
    if default_belongs_to(kind) == BelongsTo::Household {
        SUGGESTED
    } else {
        SUGGESTED_LEAVING_OUT
    }
}

/// How a kind reads in the select.
pub(super) fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Checking => "Checking",
        AccountKind::Savings => "Savings",
        AccountKind::CreditCard => "Credit card",
        AccountKind::Loan => "Loan",
        AccountKind::Mortgage => "Mortgage",
        AccountKind::Brokerage => "Brokerage",
        AccountKind::Retirement => "Retirement",
        AccountKind::Education => "Education savings (529)",
        AccountKind::Custodial => "Custodial (UTMA, UGMA)",
        AccountKind::Charitable => "Charitable (donor-advised fund)",
        AccountKind::EquityComp => "Equity compensation",
        AccountKind::Property => "Property",
        AccountKind::Vehicle => "Vehicle",
        AccountKind::OtherAsset => "Other asset",
        AccountKind::OtherLiability => "Other liability",
    }
}

fn provider_account_id(raw: &str) -> Result<ProviderAccountId, PageError> {
    raw.parse().map_err(|_| PageError::not_found())
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    belongs_to: String,
    #[serde(default)]
    beneficiary: String,
}

assert_no_debug!(CreateForm);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    account: String,
}

pub(super) async fn unprocessable(
    state: &AppState,
    session: &PageSession,
    message: &'static str,
) -> Result<Response, PageError> {
    render(
        state,
        session,
        Some(Notice::new(StatusCode::UNPROCESSABLE_ENTITY, message)),
    )
    .await
}

/// As [`unprocessable`], for a bind form of the provider account `row`.
async fn unprocessable_on_row(
    state: &AppState,
    session: &PageSession,
    message: &'static str,
    row: ProviderAccountId,
) -> Result<Response, PageError> {
    let notice = Notice::new(StatusCode::UNPROCESSABLE_ENTITY, message).on_row(row);
    render(state, session, Some(notice)).await
}

pub(super) async fn create_account(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CreateForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = provider_account_id(&id)?;
    if let Err(err) = state.sync.authorize_create_account(&actor, id).await {
        return refused_on_row(&state, &session, err, id).await;
    }
    let kind = match form.kind.as_str() {
        "" => None,
        raw => match raw.parse::<AccountKind>() {
            Ok(kind) => Some(kind),
            Err(_) => return unprocessable_on_row(&state, &session, KIND_UNKNOWN, id).await,
        },
    };
    let Ok(belongs_to) = parse_belongs_to(&form.belongs_to) else {
        return unprocessable_on_row(&state, &session, BELONGS_TO_UNKNOWN, id).await;
    };
    let name = Some(form.name.trim().to_owned()).filter(|name| !name.is_empty());
    let input = NewLinkedAccountInput {
        name,
        kind,
        liquidity: None,
        include_in_net_worth: None,
        include_in_forecast: None,
        belongs_to,
        beneficiary: Some(form.beneficiary),
    };
    match state.sync.create_account_and_bind(&actor, id, input).await {
        Ok(_) => Ok(back()),
        Err(err) => refused_on_row(&state, &session, in_form_words(err), id).await,
    }
}

pub(super) async fn link_account(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<LinkForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = provider_account_id(&id)?;
    let Ok(account) = form.account.parse::<AccountId>() else {
        return unprocessable_on_row(&state, &session, ACCOUNT_UNKNOWN, id).await;
    };
    match state.sync.bind(&actor, id, account).await {
        Ok(_) => Ok(back()),
        Err(err) => refused_on_row(&state, &session, err, id).await,
    }
}

#[cfg(test)]
mod tests;
