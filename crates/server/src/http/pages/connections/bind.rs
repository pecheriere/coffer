//! Binding a provider account from the page: create the account it feeds,
//! or link it to an open account in its currency. The forms ask the sync
//! service, which decides; the page only leaves out forms that the
//! actor's role and access could not use.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use coffer_domain::accounts::AccountKind;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_domain::policy::{
    AccountLevel, Action, authorize, authorize_account, authorize_every_account,
};
use maud::{Markup, html};
use serde::Deserialize;

use super::{Notice, back, refused, render};
use crate::http::AppState;
use crate::http::pages::layout::csrf_field;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::accounts::AccountView;
use crate::sync::connections::{
    ConnectionState, ConnectionView, NewLinkedAccountInput, ProviderAccountView,
};

use super::FormKind;

const KIND_UNKNOWN: &str = "Choose one of the account kinds in the list.";
const ACCOUNT_UNKNOWN: &str = "Choose the account to link to.";
/// The longest account name, in characters; the form's `maxlength`.
const NAME_CHARS: usize = 100;

/// What the actor could do with the binding forms, and the open accounts a
/// provider account could be linked to.
pub(super) struct Binding {
    may_create: bool,
    may_link: bool,
    open: Vec<OpenAccount>,
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
        let may_link = authorize(actor, Action::ManageConnections).is_ok()
            && authorize_every_account(actor, &access, AccountLevel::Write).is_ok();
        let may_create = may_link && authorize(actor, Action::WriteHousehold).is_ok();
        let open = accounts
            .iter()
            .filter(|account| {
                !account.closed
                    && authorize_account(actor, &access, account.id, AccountLevel::Write).is_ok()
            })
            .map(|account| OpenAccount {
                id: account.id,
                name: account.name.clone(),
                currency: account.currency.as_str(),
            })
            .collect();
        Ok(Self {
            may_create,
            may_link,
            open,
        })
    }

    /// Whether the actor could create an account for a provider account.
    pub(super) fn can_bind(&self) -> bool {
        self.may_create
    }

    /// The forms for an unbound provider account of a live connection,
    /// unfolded when `unfolded`; nothing for one that is bound or
    /// whose connection is removed.
    pub(super) fn forms(
        &self,
        connection: &ConnectionView,
        account: &ProviderAccountView,
        csrf: &str,
        unfolded: bool,
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
        html! {
            details.item-detail open[unfolded] {
                summary { "Bind to an account" }
                form method="post" action={ "/connections/provider-accounts/" (id) "/create-account" } {
                    (csrf_field(csrf))
                    label.field {
                        "New account name"
                        input .input type="text" name="name" value=(name)
                            maxlength=(NAME_CHARS) required;
                    }
                    label.field {
                        "Kind"
                        select .input name="kind" required {
                            @if account.kind_hint.is_none() {
                                option value="" selected disabled { "Choose a kind" }
                            }
                            @for kind in AccountKind::ALL {
                                option value=(kind.as_str()) selected[account.kind_hint == Some(kind)] {
                                    (kind_label(kind))
                                }
                            }
                        }
                        span.fine { "Checking and savings count as cash for planning." }
                    }
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

/// How a kind reads in the select.
fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Checking => "Checking",
        AccountKind::Savings => "Savings",
        AccountKind::CreditCard => "Credit card",
        AccountKind::Loan => "Loan",
        AccountKind::Mortgage => "Mortgage",
        AccountKind::Brokerage => "Brokerage",
        AccountKind::Retirement => "Retirement",
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    account: String,
}

async fn unprocessable(
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

pub(super) async fn create_account(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CreateForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = provider_account_id(&id)?;
    let kind = match form.kind.as_str() {
        "" => None,
        raw => match raw.parse::<AccountKind>() {
            Ok(kind) => Some(kind),
            Err(_) => return unprocessable(&state, &session, KIND_UNKNOWN).await,
        },
    };
    let name = Some(form.name.trim().to_owned()).filter(|name| !name.is_empty());
    let input = NewLinkedAccountInput {
        name,
        kind,
        liquidity: None,
        include_in_net_worth: true,
        include_in_forecast: true,
    };
    match state.sync.create_account_and_bind(&actor, id, input).await {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
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
        return unprocessable(&state, &session, ACCOUNT_UNKNOWN).await;
    };
    match state.sync.bind(&actor, id, account).await {
        Ok(_) => Ok(back()),
        Err(err) => refused(&state, &session, err, FormKind::Action).await,
    }
}
