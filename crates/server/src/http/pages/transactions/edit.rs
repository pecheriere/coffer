//! The forms that change transactions, each sent to the service as the patch
//! `PATCH /api/v1/transactions/{id}` or `POST /api/v1/transactions/batch` would carry. The form
//! is read whole before anything is sent, its anti-forgery token first; unknown fields are
//! refused and nothing typed is ever echoed.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::response::Redirect;
use coffer_domain::ids::{MemberId, TransactionId};
use coffer_domain::transactions::{ReviewStatus, SplitInput};
use serde::Deserialize;

use super::filters::Filters;
use super::splits::{self, NO_CATEGORY};
use super::{detail_path, refused, transaction_id};
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

/// Unknown fields are refused: a forged form fails whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DetailsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    assignee: String,
}

/// Sends only what differs from what the page showed: a payee sent unchanged would pin the
/// bank's one as the member's own, and an assignee who has since left would be refused.
pub(super) async fn details(
    State(state): State<AppState>,
    session: PageSession,
    Path(id): Path<String>,
    PageForm(form): PageForm<DetailsForm>,
) -> Result<Redirect, PageError> {
    apply(&state, &session, &form.csrf, &id, |current| {
        let payee = match form.payee.as_str() {
            "" => Patch::Clear,
            same if current.payee.as_deref() == Some(same) => Patch::Absent,
            other => Patch::Set(other.to_owned()),
        };
        // A browser sends a textarea's line breaks as CR LF; notes keep LF alone.
        let notes = Patch::Set(form.notes.replace("\r\n", "\n"));
        let chosen: Option<MemberId> = match form.assignee.as_str() {
            "" => None,
            text => Some(
                text.parse()
                    .map_err(|_| PageError::invalid("not a member the form offers"))?,
            ),
        };
        let assignee = match chosen {
            same if same == current.assignee => Patch::Absent,
            None => Patch::Clear,
            Some(member) => Patch::Set(member),
        };
        Ok(TransactionPatch {
            payee,
            notes,
            assignee,
            ..TransactionPatch::default()
        })
    })
    .await
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
