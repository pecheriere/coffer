//! One connection's card on the page: its status, actions, held cursor notice and provider
//! accounts. It only draws what [`Shared`] and [`Permissions`] say the viewer may use.

use std::collections::{HashMap, HashSet};

use coffer_domain::ids::{AccountId, ConnectionId, ProviderAccountId};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use maud::{Markup, html};

use super::{belonging, bind, forget};
use crate::http::pages::layout::csrf_field;
use crate::ledger::accounts::AccountView;
use crate::sync::connections::{
    ConnectionState, ConnectionView, HeldCursorView, ProviderAccountView,
};
use crate::sync::provider::ProviderKind;

/// What every connection card shares.
pub(super) struct Shared<'a> {
    /// What the viewer sees of each account. Nothing on this page reads its balance.
    pub(super) accounts: &'a HashMap<AccountId, &'a AccountView>,
    pub(super) binding: &'a bind::Binding,
    pub(super) timezone: &'a TimeZone,
    pub(super) csrf: &'a str,
    pub(super) can_manage: bool,
    pub(super) plaid_offered: bool,
    /// The connections the viewer may forget: their sealed token cannot be
    /// opened, so removal cannot give the credentials up.
    pub(super) forgettable: &'a HashSet<ConnectionId>,
    /// The provider account whose bind form this page answers a refusal of.
    pub(super) refused_row: Option<ProviderAccountId>,
}

/// What the viewer may do with one connection beyond what everyone with
/// `can_manage` may.
#[derive(Clone, Copy)]
pub(super) struct Permissions {
    pub(super) may_remove: bool,
    pub(super) may_release: bool,
}

pub(super) fn connection_card(
    connection: &ConnectionView,
    permissions: Permissions,
    shared: &Shared<'_>,
) -> Markup {
    let Permissions {
        may_remove,
        may_release,
    } = permissions;
    let Shared {
        timezone,
        csrf,
        can_manage,
        plaid_offered,
        ..
    } = *shared;
    let plaid = connection.provider == ProviderKind::Plaid.as_str();
    let live = connection.status != ConnectionState::Removed;
    let id = connection.id;
    // The accounts that wait for a binding come first, and the dismissed ones last.
    let mut accounts: Vec<&ProviderAccountView> = connection.accounts.iter().collect();
    accounts.sort_by_key(|account| (account.dismissed_at.is_some(), account.account.is_some()));
    let waiting = live
        && shared.binding.can_bind()
        && accounts
            .first()
            .is_some_and(|first| first.account.is_none() && first.dismissed_at.is_none());
    html! {
        section.card {
            h2 { (connection.label) }
            div.item {
                div.item-main {
                    span.item-meta {
                        (connection.provider) " · Last synced "
                        (moment(connection.last_success_at, timezone))
                        " · Last attempt " (moment(connection.last_attempt_at, timezone))
                        @if let Some(next) = connection.next_sync_at {
                            " · Next sync " (moment(Some(next), timezone))
                        }
                    }
                    @if let Some(error) = &connection.last_error {
                        span.item-meta { "Error: " (error) }
                    }
                }
                (status_chip(connection.status))
                @if live && (can_manage || may_remove) {
                    div.item-actions {
                        @if can_manage {
                            form method="post" action={ "/connections/" (id) "/sync" } {
                                (csrf_field(csrf))
                                button.btn type="submit" { "Sync now" }
                            }
                        }
                        @if may_remove {
                            form method="post" action={ "/connections/" (id) "/remove" } {
                                (csrf_field(csrf))
                                button.btn type="submit" { "Remove" }
                            }
                        }
                    }
                }
            }
            @if let Some(held) = &connection.held_cursor {
                (held_notice(connection, held, may_release, csrf))
            }
            @if may_remove && connection.status == ConnectionState::ReauthRequired {
                (reconnect_section(id, plaid, plaid_offered, csrf))
            }
            @if live && plaid && !plaid_offered && !shared.forgettable.contains(&id) {
                p.fine {
                    "Plaid is not configured on this server. Configure it again to remove this connection."
                }
            }
            @if shared.forgettable.contains(&id) {
                (forget::section(id, &connection.label, csrf))
            }
            @if waiting {
                h3 { "Set up your accounts" }
                p {
                    "coffer found these bank accounts. Until each is bound to an account, "
                    "its transactions are held. Name each one and choose its kind, or link "
                    "it to an account you already have."
                }
            } @else {
                h3 { "Accounts" }
            }
            @if accounts.is_empty() {
                p.fine { "No accounts reported yet." }
            } @else {
                ul.items {
                    @for (index, account) in accounts.iter().enumerate() {
                        (belonging::account_item(connection, account, shared, waiting && index == 0))
                    }
                }
            }
        }
    }
}

/// What a connection that needs a new login asks of who may remove it. Plaid's
/// login happens on its own page, so a server not configured for it has none
/// to offer.
fn reconnect_section(id: ConnectionId, plaid: bool, plaid_offered: bool, csrf: &str) -> Markup {
    html! {
        @if plaid && plaid_offered {
            h3 { "Reconnect" }
            p { "Plaid needs a new login to this institution; its accounts stay bound." }
            a.btn.btn-primary href={ "/connections/" (id) "/plaid/reconnect" } { "Reconnect" }
        }
        @if !plaid {
            h3 { "Reconnect" }
            p { "SimpleFin no longer accepts this connection's access. Paste a new setup token to restore it; its accounts stay bound." }
            form method="post" action={ "/connections/" (id) "/simplefin/reconnect" } {
                (csrf_field(csrf))
                label.field {
                    "New setup token"
                    input .input type="password" name="setup_token" autocomplete="off"
                        required;
                }
                button.btn.btn-primary type="submit" { "Reconnect" }
            }
        }
    }
}

/// The notice of a held sync cursor: why, what letting it move on loses and,
/// for who may, the form that does.
fn held_notice(
    connection: &ConnectionView,
    held: &HeldCursorView,
    may_release: bool,
    csrf: &str,
) -> Markup {
    let accounts = held
        .provider_accounts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    html! {
        div.once.held-cursor {
            h3 { "Sync cursor held" }
            p { (held.text) }
            ul {
                @for account in connection.accounts.iter().filter(|account| held.provider_accounts.contains(&account.id)) {
                    li { (account.name) }
                }
            }
            @if held.release_withdrawn {
                p {
                    "Your earlier release was withdrawn: what is wrong with these accounts, or "
                    "how many records they have, changed after it was asked for, so nothing was "
                    "passed. Look at them again. "
                    "The cursor stays held unless the release is asked for again."
                }
            }
            @if held.release_pending || may_release {
                p {
                    "The last fetch held " (held.records) " records of these accounts that could "
                    "not be stored."
                }
            }
            @if held.release_pending {
                p {
                    "Release requested: a sync moves the cursor on as soon as no other account "
                    "blocks it and the failing records can be tried. If a fetch brings more "
                    "records than this, the request is withdrawn and you are asked again."
                }
            } @else if may_release {
                p {
                    "If you let the cursor move on, the next sync tries each of them one by one. "
                    "Those that can be stored are stored. Those that still fail are passed for "
                    "good: coffer does not fetch them again, they are only counted as skipped in "
                    "that sync, and the audit log records how many. Whatever they carry (new "
                    "transactions, changes, removals) is lost, unless you enter it by hand. "
                    "If another account still blocks the cursor, nothing is passed yet and "
                    "your request waits."
                }
                p {
                    "If an account's own error (shown beside it) is what holds the cursor, for "
                    "instance one that cannot be read, nothing of it can be tried: while that "
                    "error lasts, all its records are passed for good untried."
                }
                form method="post" action={ "/connections/" (connection.id) "/release-cursor" } {
                    (csrf_field(csrf))
                    input type="hidden" name="accounts" value=(accounts);
                    input type="hidden" name="records" value=(held.records);
                    button.btn type="submit" { "Pass the failing records for good" }
                }
            }
        }
    }
}

fn status_chip(status: ConnectionState) -> Markup {
    let text = match status {
        ConnectionState::Active => "active",
        ConnectionState::Error => "error",
        ConnectionState::ReauthRequired => "reconnect needed",
        ConnectionState::Removed => "removed",
    };
    html! {
        @if status == ConnectionState::Active {
            span.chip { (text) }
        } @else {
            span.chip.chip-quiet { (text) }
        }
    }
}

/// `at` in the household's time zone, to the minute; "never" without one.
fn moment(at: Option<Timestamp>, timezone: &TimeZone) -> String {
    at.map_or_else(
        || "never".to_owned(),
        |at| {
            at.to_zoned(timezone.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}
