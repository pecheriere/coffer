//! The forms that change transactions, each sent to the service as the patch
//! `PATCH /api/v1/transactions/{id}` or `POST /api/v1/transactions/batch` would carry. The form
//! is read whole before anything is sent, its anti-forgery token first; unknown fields are
//! refused and nothing typed is ever echoed.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use coffer_domain::ids::{MemberId, TransactionId};
use coffer_domain::transactions::{ReviewStatus, SplitInput};
use serde::Deserialize;

use super::filters::Filters;
use super::splits::{self, NO_CATEGORY};
use super::{detail, detail_path, refused, transaction_id};
use crate::http::AppState;
use crate::http::pages::{PageError, PageForm, PageSession};
use crate::ledger::transactions::{BatchOperation, Patch, TransactionPatch, TransactionView};

/// The most transactions one batch changes, as the API takes.
const MAX_BATCH: usize = 200;

fn review_choice(text: &str) -> Result<ReviewStatus, PageError> {
    text.parse()
        .map_err(|_| PageError::invalid("not an answer the form offers"))
}

/// A checkbox: unticked sends nothing, ticked sends `yes`.
fn ticked(text: &str) -> Result<bool, PageError> {
    match text {
        "" => Ok(false),
        "yes" => Ok(true),
        _ => Err(PageError::invalid("not an answer the form offers")),
    }
}

async fn apply(
    state: &AppState,
    session: &PageSession,
    csrf: &str,
    id: &str,
    patch: impl FnOnce(&TransactionView) -> Result<TransactionPatch, PageError>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(state, csrf)?;
    let id = transaction_id(id)?;
    // The current state, for a patch built from it; read as the API would, so a transaction
    // the actor can't see is missing here too.
    let current = state
        .ledger
        .get_transaction(&actor, id)
        .await
        .map_err(refused)?;
    let patch = patch(&current)?;
    state
        .ledger
        .update_transaction(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)))
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    review: String,
}

pub(super) async fn review(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<ReviewForm>,
) -> Result<Redirect, PageError> {
    apply(&state, &session, &form.csrf, &id, |_| {
        Ok(TransactionPatch {
            review: Some(review_choice(&form.review)?),
            ..TransactionPatch::default()
        })
    })
    .await
}

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CategoryForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    reviewed: String,
}

/// Files the whole amount in one category, or none, and marks it reviewed when asked.
pub(super) async fn category(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<CategoryForm>,
) -> Result<Redirect, PageError> {
    apply(&state, &session, &form.csrf, &id, |current| {
        let splits = match form.category.as_str() {
            "" => return Err(PageError::invalid("choose a category")),
            NO_CATEGORY => Vec::new(),
            text => vec![SplitInput {
                category: text
                    .parse()
                    .map_err(|_| PageError::invalid("not a category the form offers"))?,
                minor_units: current.amount.minor_units,
                goal: None,
            }],
        };
        let review = ticked(&form.reviewed)?.then_some(ReviewStatus::Reviewed);
        Ok(TransactionPatch {
            splits: Some(splits),
            review,
            ..TransactionPatch::default()
        })
    })
    .await
}

/// The splits form: its token and its rows, see [`splits::parse`].
#[derive(Deserialize)]
pub(super) struct SplitsForm {
    #[serde(default)]
    csrf: String,
    #[serde(flatten)]
    rows: BTreeMap<String, String>,
}

pub(super) async fn splits(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<SplitsForm>,
) -> Result<Redirect, PageError> {
    apply(&state, &session, &form.csrf, &id, |current| {
        Ok(TransactionPatch {
            splits: Some(splits::parse(current, &form.rows)?),
            ..TransactionPatch::default()
        })
    })
    .await
}

/// Unknown fields are refused: a forged form fails whole. Each `_was` field holds what the page
/// showed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DetailsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    payee_was: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    notes_was: String,
    #[serde(default)]
    assignee: String,
    #[serde(default)]
    assignee_was: String,
}

/// What the details form says when a field it changed has changed since the page opened.
const DETAILS_CHANGED: &str =
    "It changed since you opened it, so nothing was saved. Here is what it says now.";

/// A browser sends a textarea's line breaks as CR LF; notes keep LF alone.
fn plain_lines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// A member picked in the form, or nobody.
fn picked_member(text: &str) -> Result<Option<MemberId>, PageError> {
    match text {
        "" => Ok(None),
        text => text
            .parse()
            .map(Some)
            .map_err(|_| PageError::invalid("not a member the form offers")),
    }
}

/// One field of the details form: what the page showed, what came back, and what it is now.
struct Field<T> {
    was: T,
    sent: T,
    now: T,
}

impl<T: PartialEq> Field<T> {
    /// The value to send, or `None` when it was left as shown. `Err` when it was changed here
    /// and has changed meanwhile too, so saving would undo a change nobody here saw.
    fn change(self) -> Result<Option<T>, Stale> {
        if self.sent == self.was {
            Ok(None)
        } else if self.now == self.was {
            Ok(Some(self.sent))
        } else {
            Err(Stale)
        }
    }
}

/// A field changed both in the form and since the page opened.
struct Stale;

/// The patch for what the form changed, compared with what the page showed, or `None` when a
/// field it changed has changed meanwhile too. A field left as shown is not sent, so a payee a
/// sync changed meanwhile isn't pinned, nor an assignee a rule set undone, nor notes someone
/// else wrote overwritten.
fn details_patch(
    form: &DetailsForm,
    current: &TransactionView,
) -> Result<Option<TransactionPatch>, PageError> {
    let assignee = Field {
        was: picked_member(&form.assignee_was)?,
        sent: picked_member(&form.assignee)?,
        now: current.assignee,
    };
    let payee = Field {
        was: form.payee_was.as_str(),
        sent: form.payee.as_str(),
        now: current.payee.as_deref().unwrap_or_default(),
    };
    let notes = Field {
        was: plain_lines(&form.notes_was),
        sent: plain_lines(&form.notes),
        now: plain_lines(current.notes.as_deref().unwrap_or_default()),
    };
    Ok(changes(payee, notes, assignee).ok())
}

fn changes(
    payee: Field<&str>,
    notes: Field<String>,
    assignee: Field<Option<MemberId>>,
) -> Result<TransactionPatch, Stale> {
    Ok(TransactionPatch {
        payee: match payee.change()? {
            None => Patch::Absent,
            Some("") => Patch::Clear,
            Some(other) => Patch::Set(other.to_owned()),
        },
        notes: match notes.change()? {
            None => Patch::Absent,
            Some(text) if text.is_empty() => Patch::Clear,
            Some(text) => Patch::Set(text),
        },
        assignee: match assignee.change()? {
            None => Patch::Absent,
            Some(None) => Patch::Clear,
            Some(Some(member)) => Patch::Set(member),
        },
        ..TransactionPatch::default()
    })
}

/// Sends only what the form changed from what the page showed. A field changed here that also
/// changed since the page opened is refused whole, and the page shown again as it is now.
pub(super) async fn details(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<DetailsForm>,
) -> Result<Response, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let id = transaction_id(&id)?;
    let current = state
        .ledger
        .get_transaction(&actor, id)
        .await
        .map_err(refused)?;
    let Some(patch) = details_patch(&form, &current)? else {
        return detail::render(&state, &session, id, Some(DETAILS_CHANGED)).await;
    };
    state
        .ledger
        .update_transaction(&actor, id, patch)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&detail_path(id)).into_response())
}

/// The batch form: its token, which way to mark, the list to go back to, and a `pick_<id>`
/// checkbox for each transaction ticked.
#[derive(Deserialize)]
pub(super) struct BatchForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    review: String,
    #[serde(default)]
    back: String,
    #[serde(flatten)]
    picks: BTreeMap<String, String>,
}

/// The transactions a batch form ticked. Anything but a `pick_<id>` field set to `yes` is
/// refused whole.
fn picked(picks: &BTreeMap<String, String>) -> Result<Vec<TransactionId>, PageError> {
    let unreadable = || PageError::invalid("the form could not be read");
    let ids = picks
        .iter()
        .map(|(name, value)| {
            let id = name.strip_prefix("pick_").ok_or_else(unreadable)?;
            if value != "yes" {
                return Err(unreadable());
            }
            id.parse().map_err(|_| unreadable())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Err(PageError::invalid("tick at least one transaction"));
    }
    if ids.len() > MAX_BATCH {
        return Err(PageError::invalid("tick at most 200 transactions at once"));
    }
    Ok(ids)
}

/// Marks every ticked transaction reviewed, or to review, all together or not at all, then
/// goes back to the list with the filters it showed.
pub(super) async fn batch(
    State(state): State<AppState>,
    session: PageSession,
    PageForm(form): PageForm<BatchForm>,
) -> Result<Redirect, PageError> {
    let actor = session.verify(&state, &form.csrf)?;
    let review = review_choice(&form.review)?;
    let (back, _) = Filters::parse(Some(&form.back))?;
    let operations = picked(&form.picks)?
        .into_iter()
        .map(|id| BatchOperation {
            id,
            patch: TransactionPatch {
                review: Some(review),
                ..TransactionPatch::default()
            },
        })
        .collect();
    state
        .ledger
        .apply_batch(&actor, operations)
        .await
        .map_err(refused)?;
    Ok(Redirect::to(&back.path()))
}
