//! `/api/v1/transactions`: listing and searching transactions, creating
//! manual ones, reviewing them one at a time or in atomic batches, and
//! resolving possible duplicates (merge, unmerge, dismiss).

use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::ids::{AccountId, CategoryId, MemberId, SourceRecordId, TransactionId};
use coffer_domain::policy::AccountLevel;
use coffer_domain::transactions::{ReviewStatus, TransactionFlag};
use coffer_store::{AssigneeFilter, PageRequest};
use jiff::civil::Date;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::transactions::{
    BatchOperation, NewManualTransactionInput, TransactionPage, TransactionPatch,
    TransactionQueryInput, TransactionView,
};

/// The most ids a comma-separated query parameter may hold.
const MAX_LISTED_IDS: usize = 100;
/// The most characters of a text search.
const MAX_SEARCH_CHARS: usize = 100;

/// A comma-separated list of 1 to [`MAX_LISTED_IDS`] values. Rejections
/// carry a fixed message: the parameter is named by the extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed<T>(Vec<T>);

impl<'de, T: FromStr> Deserialize<'de> for Listed<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let values = text
            .split(',')
            .map(|part| part.parse::<T>().ok())
            .collect::<Option<Vec<T>>>()
            .ok_or_else(|| D::Error::custom("not a list of ids"))?;
        if values.len() > MAX_LISTED_IDS {
            return Err(D::Error::custom("too many ids"));
        }
        Ok(Self(values))
    }
}

/// `none`, or a member id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Assignee(AssigneeFilter);

impl<'de> Deserialize<'de> for Assignee {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == "none" {
            return Ok(Self(AssigneeFilter::Unassigned));
        }
        text.parse::<MemberId>()
            .map(|member| Self(AssigneeFilter::Member(member)))
            .map_err(|_| D::Error::custom("not a member id"))
    }
}

/// 1 to [`MAX_SEARCH_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchText(String);

impl<'de> Deserialize<'de> for SearchText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if !(1..=MAX_SEARCH_CHARS).contains(&text.chars().count()) {
            return Err(D::Error::custom("search text length"));
        }
        Ok(Self(text))
    }
}

/// Which transactions to list. Every parameter given must hold.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct TransactionQuery {
    /// Only these accounts: 1 to 100 comma-separated account ids. An
    /// account that does not exist, or that the token cannot see, is a 404.
    #[param(value_type = Option<String>)]
    account: Option<Listed<AccountId>>,
    /// Only transactions posted on or after this day (ISO 8601).
    from: Option<Date>,
    /// Only transactions posted before this day (ISO 8601).
    to: Option<Date>,
    /// Only reviewed, or only unreviewed, transactions.
    review: Option<ReviewStatus>,
    /// Only transactions assigned to this member (an id), or to nobody
    /// (`none`).
    #[param(value_type = Option<String>)]
    assignee: Option<Assignee>,
    /// Only transactions with a split in one of these categories: 1 to 100
    /// comma-separated category ids.
    #[param(value_type = Option<String>)]
    category: Option<Listed<CategoryId>>,
    /// `true`: only transactions without splits. `false` does not filter.
    uncategorized: Option<bool>,
    /// Only pending (`true`) or only posted (`false`) transactions.
    pending: Option<bool>,
    /// Only transactions with this flag.
    flag: Option<TransactionFlag>,
    /// Only signed amounts of at least this many minor units (negative is
    /// money out).
    min_amount: Option<i64>,
    /// Only signed amounts of at most this many minor units.
    max_amount: Option<i64>,
    /// 1 to 100 characters contained in the description, the payee or the
    /// notes. Case is ignored for ASCII letters only; `%`, `_` and `\` are
    /// matched literally.
    #[param(value_type = Option<String>, min_length = 1, max_length = 100)]
    q: Option<SearchText>,
    /// The `next` cursor of the previous page.
    after: Option<String>,
    /// The most transactions to return, clamped to 1 to 200 (default 50).
    #[param(minimum = 0)]
    limit: Option<u32>,
}

impl TransactionQuery {
    fn into_input(self) -> TransactionQueryInput {
        let mut page = PageRequest {
            after: self.after,
            ..PageRequest::default()
        };
        if let Some(limit) = self.limit {
            page.limit = limit;
        }
        TransactionQueryInput {
            accounts: self.account.map(|Listed(ids)| ids).unwrap_or_default(),
            from: self.from,
            to: self.to,
            review: self.review,
            assignee: self.assignee.map(|Assignee(filter)| filter),
            categories: self.category.map(|Listed(ids)| ids).unwrap_or_default(),
            uncategorized: self.uncategorized.unwrap_or(false),
            pending: self.pending,
            flag: self.flag,
            min_minor: self.min_amount,
            max_minor: self.max_amount,
            text: self.q.map(|SearchText(text)| text),
            page,
        }
    }
}

/// One transaction.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct TransactionPath {
    /// The transaction's id.
    pub transaction_id: TransactionId,
}

/// A manual transaction to create.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTransactionRequest {
    /// An open manual account; the transaction takes its currency.
    pub account: AccountId,
    /// The calendar day it posted (ISO 8601).
    pub posted_on: Date,
    /// The amount in the account currency's minor units; negative is money
    /// out.
    pub minor_units: i64,
    /// 1 to 500 characters, without control characters.
    pub description: String,
    /// 1 to 200 characters, without control characters.
    pub payee: Option<String>,
    /// 1 to 500 characters, without control characters.
    pub memo: Option<String>,
    /// At most 2000 characters; newlines are the only control characters
    /// allowed.
    pub notes: Option<String>,
}

/// Changes to several transactions, applied together.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchRequest {
    /// 1 to 200 operations, each naming a different transaction.
    pub operations: Vec<BatchOperation>,
}

/// A transaction to merge into the one named in the path.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeRequest {
    /// An imported transaction of the same account; it is deleted.
    pub duplicate: TransactionId,
    /// Merge even though the duplicate has notes, splits, a review, an
    /// assignee or an edited payee, discarding them (default `false`).
    #[serde(default)]
    pub discard_duplicate_state: bool,
}

/// A source record to move into a transaction of its own.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnmergeRequest {
    /// One of the transaction's `sources`, not its only one.
    pub source: SourceRecordId,
}

/// Transactions, in the order of the request's operations.
#[derive(Debug, Serialize, ToSchema)]
pub struct TransactionList {
    pub items: Vec<TransactionView>,
}

/// Lists transactions.
#[utoipa::path(
    get,
    path = "/api/v1/transactions",
    operation_id = "listTransactions",
    tag = tags::TRANSACTIONS,
    description = "The transactions of every account the token may see, newest posting day \
                   first (ties newest first), each with its splits. Every filter given must \
                   hold. Needs `read`. Pages with the opaque `next` cursor.",
    security(("bearer" = [])),
    params(TransactionQuery),
    responses(
        (status = 200, description = "One page of transactions.", body = TransactionPage),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedQuery(query): TypedQuery<TransactionQuery>,
) -> Result<Json<TransactionPage>, Problem> {
    Ok(Json(
        state
            .ledger
            .list_transactions(&actor, query.into_input())
            .await?,
    ))
}

/// Creates a manual transaction.
#[utoipa::path(
    post,
    path = "/api/v1/transactions",
    operation_id = "createTransaction",
    tag = tags::TRANSACTIONS,
    description = "Creates a manual transaction, unreviewed and uncategorized, in an open \
                   account. Needs `write` on the account. With an `Idempotency-Key`, a retry \
                   of the same request within 24 hours returns the first response instead of \
                   creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        member for 24 hours."),
    ),
    request_body = NewTransactionRequest,
    responses(
        (status = 201, description = "The transaction, created now or by an earlier request \
                                      with the same `Idempotency-Key`.",
         body = TransactionView),
        (status = 409, description = "The account is closed, or a request with this \
                                      `Idempotency-Key` is still in progress.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn create(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    method: Method,
    uri: Uri,
    key: Option<IdempotencyKey>,
    TypedJsonBytes { value, bytes }: TypedJsonBytes<NewTransactionRequest>,
) -> Result<Response, Problem> {
    let account = value.account;
    let input = NewManualTransactionInput {
        account,
        posted_on: value.posted_on,
        minor_units: value.minor_units,
        description: value.description,
        payee: value.payee,
        memo: value.memo,
        notes: value.notes,
    };
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || {
        Ok(state
            .ledger
            .authorize_account_access(&actor, account, AccountLevel::Write)
            .await?)
    };
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state
            .ledger
            .create_manual_transaction(&actor, input, slot)
            .await
    })
    .await
}

/// One transaction.
#[utoipa::path(
    get,
    path = "/api/v1/transactions/{transaction_id}",
    operation_id = "getTransaction",
    tag = tags::TRANSACTIONS,
    description = "One transaction with its splits. Needs `read`.",
    security(("bearer" = [])),
    params(TransactionPath),
    responses(
        (status = 200, description = "The transaction.", body = TransactionView),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
) -> Result<Json<TransactionView>, Problem> {
    Ok(Json(
        state
            .ledger
            .get_transaction(&actor, path.transaction_id)
            .await?,
    ))
}

/// Changes a transaction.
#[utoipa::path(
    patch,
    path = "/api/v1/transactions/{transaction_id}",
    operation_id = "updateTransaction",
    tag = tags::TRANSACTIONS,
    description = "Changes the fields given: payee, notes, review, assignee and splits. \
                   Marking it reviewed records who reviewed it; changing the splits records \
                   who categorized it. A patch that changes nothing records nothing. Needs \
                   `write` on its account.",
    security(("bearer" = [])),
    params(TransactionPath),
    request_body = TransactionPatch,
    responses(
        (status = 200, description = "The transaction as it now is.", body = TransactionView),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
    TypedJson(changes): TypedJson<TransactionPatch>,
) -> Result<Json<TransactionView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_transaction(&actor, path.transaction_id, changes)
            .await?,
    ))
}

/// Deletes a manual transaction.
#[utoipa::path(
    delete,
    path = "/api/v1/transactions/{transaction_id}",
    operation_id = "deleteTransaction",
    tag = tags::TRANSACTIONS,
    description = "Deletes a manual transaction and its splits. Imported transactions are \
                   never deleted by hand. Needs `write` on its account.",
    security(("bearer" = [])),
    params(TransactionPath),
    responses(
        (status = 204, description = "The transaction is deleted."),
        (status = 409, description = "The transaction is imported.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
) -> Result<StatusCode, Problem> {
    state
        .ledger
        .delete_transaction(&actor, path.transaction_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Changes several transactions at once.
#[utoipa::path(
    post,
    path = "/api/v1/transactions/batch",
    operation_id = "batchUpdateTransactions",
    tag = tags::TRANSACTIONS,
    description = "Applies 1 to 200 patches, each to a different transaction, in order and \
                   atomically: all of them or none. Each is authorized, recorded and \
                   attributed like `PATCH /api/v1/transactions/{transaction_id}`. When one \
                   fails, the problem has its status and code, and its `detail` starts with \
                   `operations[<index>]: `. Needs `write` on every account involved.",
    security(("bearer" = [])),
    request_body = BatchRequest,
    responses(
        (status = 200, description = "The transactions as they now are, in operation order.",
         body = TransactionList),
        CommonProblems,
    ),
)]
pub(super) async fn batch(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedJson(body): TypedJson<BatchRequest>,
) -> Result<Json<TransactionList>, Problem> {
    let items = state.ledger.apply_batch(&actor, body.operations).await?;
    Ok(Json(TransactionList { items }))
}

/// Merges a duplicate into a transaction.
#[utoipa::path(
    post,
    path = "/api/v1/transactions/{transaction_id}/merge",
    operation_id = "mergeTransactions",
    tag = tags::TRANSACTIONS,
    description = "Merges `duplicate` into this transaction, both imported and of the same \
                   account: the duplicate's source records move here and the duplicate is \
                   deleted. This transaction keeps its notes, splits, review, assignee and \
                   payee, so choose the one holding them as the target; a pending target \
                   merged with a posted duplicate takes its posted fields. The duplicate's \
                   own user state is never combined: unless `discard_duplicate_state` is \
                   `true`, a duplicate that has any is refused with `merge_conflict`, and \
                   discarded values are recorded in the audit log. Its candidacies move \
                   here, and `possible_duplicate` clears once no candidate is left. Two \
                   transactions that each hold a posted record of the same connection are \
                   distinct purchases and are refused. Needs `write` on the account.",
    security(("bearer" = [])),
    params(TransactionPath),
    request_body = MergeRequest,
    responses(
        (status = 200, description = "The merged transaction as it now is.",
         body = TransactionView),
        (status = 409, description = "`merge_conflict`: the duplicate has user state and \
                                      `discard_duplicate_state` is not `true`.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn merge(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
    TypedJson(body): TypedJson<MergeRequest>,
) -> Result<Json<TransactionView>, Problem> {
    Ok(Json(
        state
            .ledger
            .merge(
                &actor,
                path.transaction_id,
                body.duplicate,
                body.discard_duplicate_state,
            )
            .await?,
    ))
}

/// Moves a source record into a transaction of its own.
#[utoipa::path(
    post,
    path = "/api/v1/transactions/{transaction_id}/unmerge",
    operation_id = "unmergeTransaction",
    tag = tags::TRANSACTIONS,
    description = "Moves one of this transaction's source records into a new imported \
                   transaction of the same account, with the record's fields and no notes, \
                   splits, review, assignee or flags. This transaction keeps its user state \
                   and shows the fields of its remaining records; a single split follows a \
                   new amount. Later syncs keep them apart. Needs `write` on the account. \
                   With an `Idempotency-Key`, a retry of the same request within 24 hours \
                   returns the first response.",
    security(("bearer" = [])),
    params(
        TransactionPath,
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this request, unique per \
                        member for 24 hours."),
    ),
    request_body = UnmergeRequest,
    responses(
        (status = 201, description = "The new transaction, created now or by an earlier \
                                      request with the same `Idempotency-Key`.",
         body = TransactionView),
        (status = 409, description = "The record is the transaction's only one, or a \
                                      request with this `Idempotency-Key` is still in \
                                      progress.",
         body = ProblemBody, content_type = "application/problem+json"),
        CommonProblems,
    ),
)]
pub(super) async fn unmerge(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
    method: Method,
    uri: Uri,
    key: Option<IdempotencyKey>,
    TypedJsonBytes { value, bytes }: TypedJsonBytes<UnmergeRequest>,
) -> Result<Response, Problem> {
    let transaction = path.transaction_id;
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || {
        Ok(state
            .ledger
            .authorize_transaction_access(&actor, transaction, AccountLevel::Write)
            .await?)
    };
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state
            .ledger
            .unmerge(&actor, transaction, value.source, slot)
            .await
    })
    .await
}

/// Dismisses a possible duplicate.
#[utoipa::path(
    post,
    path = "/api/v1/transactions/{transaction_id}/dismiss-duplicate",
    operation_id = "dismissDuplicate",
    tag = tags::TRANSACTIONS,
    description = "Clears the `possible_duplicate` flag and forgets the duplicate \
                   candidates: the transaction is not a duplicate of them, and a candidate \
                   listing it back forgets it too. Dismissing a \
                   transaction that is not flagged changes and records nothing. Needs \
                   `write` on the account.",
    security(("bearer" = [])),
    params(TransactionPath),
    responses(
        (status = 200, description = "The transaction as it now is.", body = TransactionView),
        CommonProblems,
    ),
)]
pub(super) async fn dismiss_duplicate(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
) -> Result<Json<TransactionView>, Problem> {
    Ok(Json(
        state
            .ledger
            .dismiss_duplicate(&actor, path.transaction_id)
            .await?,
    ))
}
