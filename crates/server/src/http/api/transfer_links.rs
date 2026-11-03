//! `/api/v1/transfer-links`: saying that two transactions are one movement of money, and
//! finding what might be the other leg of one (spec §8.12).

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::ids::{TransactionId, TransferLinkId};
use coffer_domain::policy::AccountLevel;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::extract::{TypedJsonBytes, TypedPath};
use super::openapi::tags;
use super::transactions::TransactionPath;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::transfer_links::{TransferCandidates, TransferLinkView};

/// Two transactions to link.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTransferLinkRequest {
    /// The leg with money going out of its account.
    pub outflow: TransactionId,
    /// The leg with money coming into its account, on another account.
    pub inflow: TransactionId,
}

/// One transfer link.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct TransferLinkPath {
    /// The link's id.
    pub transfer_link_id: TransferLinkId,
}

/// Links two transactions.
#[utoipa::path(
    post,
    path = "/api/v1/transfer-links",
    operation_id = "createTransferLink",
    tag = tags::TRANSACTIONS,
    description = "Says that two transactions are one movement of money: `outflow` left one \
                   account and `inflow` reached another. Coffer never does this by itself; see \
                   `listTransferCandidates` for suggestions to confirm. Their amounts may differ, \
                   as for a fee, and `difference` says by how much, and they may be in two \
                   currencies, when it is `null`. Each keeps its category.\n\n\
                   When both accounts are in counted cash (open, switched into the forecast, of a \
                   spendable class), what moved is one movement in reports and the forecast: \
                   neither spending nor income, whatever it is filed as, so it lowers no budget \
                   and pays no bill. Only the smaller leg's amount moved: a difference in one \
                   currency stays what its leg is filed as. Any other link, to a brokerage, a \
                   loan, a closed account or one that isn't the household's, stays spending and \
                   changes no number. Any link takes what it says moved out of `uncategorized`. \
                   `one_movement` says which, and follows the accounts. A leg its provider takes \
                   back suspends this: the money counts as filed again, with the caution \
                   `transfer_leg_removed`.\n\n\
                   A transaction is in one link at most. One with more than one split is refused, \
                   since part of it may be spending, and so is one linked to a goal when both \
                   accounts are in counted cash. Needs `write` on both accounts; with one hidden, \
                   this answers as for a missing transaction. With an `Idempotency-Key`, a retry \
                   of the same request within 24 hours returns the first response.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        token (or session) for 24 hours."),
    ),
    request_body = NewTransferLinkRequest,
    responses(
        (status = 201, description = "The link, made now or by an earlier request with the same \
                                      `Idempotency-Key`.",
         body = TransferLinkView),
        (status = 409, description = "A transaction is already in a link, or a request with this \
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
    TypedJsonBytes { value, bytes }: TypedJsonBytes<NewTransferLinkRequest>,
) -> Result<Response, Problem> {
    let (outflow, inflow) = (value.outflow, value.inflow);
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let authorize = async || {
        for leg in [outflow, inflow] {
            state
                .ledger
                .authorize_transaction_access(&actor, leg, AccountLevel::Write)
                .await?;
        }
        Ok(())
    };
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state
            .ledger
            .create_transfer_link(&actor, outflow, inflow, slot)
            .await
    })
    .await
}

/// One transfer link.
#[utoipa::path(
    get,
    path = "/api/v1/transfer-links/{transfer_link_id}",
    operation_id = "getTransferLink",
    tag = tags::TRANSACTIONS,
    description = "One link, with what each leg says now: a provider may have revised an amount \
                   since, and a leg it took back is flagged `source_removed`, for a person to \
                   decide about. A transaction names its link in `transfer_link`. Needs `read`, at \
                   `transactions` on both accounts; with one hidden, this answers as for a missing \
                   link.",
    security(("bearer" = [])),
    params(TransferLinkPath),
    responses(
        (status = 200, description = "The link.", body = TransferLinkView),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransferLinkPath>,
) -> Result<Json<TransferLinkView>, Problem> {
    Ok(Json(
        state
            .ledger
            .get_transfer_link(&actor, path.transfer_link_id)
            .await?,
    ))
}

/// Removes a transfer link.
#[utoipa::path(
    delete,
    path = "/api/v1/transfer-links/{transfer_link_id}",
    operation_id = "removeTransferLink",
    tag = tags::TRANSACTIONS,
    description = "Ends the link: both transactions stay as they are, and count again as what they \
                   are filed as. The audit log keeps what was joined, so it can be made again. \
                   Needs `write` on both accounts; with one hidden, this answers as for a missing \
                   link.",
    security(("bearer" = [])),
    params(TransferLinkPath),
    responses(
        (status = 204, description = "The link is gone."),
        CommonProblems,
    ),
)]
pub(super) async fn remove(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransferLinkPath>,
) -> Result<StatusCode, Problem> {
    state
        .ledger
        .remove_transfer_link(&actor, path.transfer_link_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Transactions that might be the other leg of a transfer.
#[utoipa::path(
    get,
    path = "/api/v1/transactions/{transaction_id}/transfer-candidates",
    operation_id = "listTransferCandidates",
    tag = tags::TRANSACTIONS,
    description = "What might be the other leg of this transaction, for a person or agent to \
                   confirm with `createTransferLink`: the same amount the other way, in the same \
                   currency, on another account the token can read, posted within 5 days, in no \
                   link and with at most one split, nearest first. Empty for a transaction that \
                   can't be linked itself. A leg of another amount or currency is never suggested, \
                   and may still be linked. It changes nothing. Needs `read`.",
    security(("bearer" = [])),
    params(TransactionPath),
    responses(
        (status = 200, description = "The candidates.", body = TransferCandidates),
        CommonProblems,
    ),
)]
pub(super) async fn candidates(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<TransactionPath>,
) -> Result<Json<TransferCandidates>, Problem> {
    Ok(Json(
        state
            .ledger
            .transfer_candidates(&actor, path.transaction_id)
            .await?,
    ))
}
