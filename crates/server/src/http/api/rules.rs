//! `/api/v1/rules`: rules that file or assign transactions nobody has filled yet.

use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::Response;
use coffer_domain::ids::RuleId;
use serde::Deserialize;
use utoipa::IntoParams;

use super::extract::{TypedJson, TypedJsonBytes, TypedPath, TypedQuery};
use super::openapi::tags;
use crate::http::AppState;
use crate::http::extract::ApiActor;
use crate::http::idempotency::{self, CreateRequest, IdempotencyKey};
use crate::http::problem::{CommonProblems, Problem, ProblemBody};
use crate::ledger::rules::run::{RuleApplied, RuleDryRun};
use crate::ledger::rules::undo::RuleUndone;
use crate::ledger::rules::{RuleInput, RuleList, RulePatch, RuleView};

/// One rule.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
#[serde(deny_unknown_fields)]
pub struct RulePath {
    /// The rule's id.
    pub rule_id: RuleId,
}

/// How many changes a dry run lists.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DryRunQuery {
    /// The most changes to list, clamped to 1 to 200; by default 50. The counts cover
    /// them all.
    #[param(minimum = 0)]
    pub limit: Option<u32>,
}

/// The rules.
#[utoipa::path(
    get,
    path = "/api/v1/rules",
    operation_id = "listRules",
    tag = tags::RULES,
    description = "The rules the token may see, in the order they run. Needs `read`; a token \
                   with account grants sees a rule for one account with `transactions` on it, \
                   and a rule for every account with `transactions` on any account.",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The rules.", body = RuleList),
        CommonProblems,
    ),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
) -> Result<Json<RuleList>, Problem> {
    Ok(Json(state.ledger.list_rules(&actor).await?))
}

/// Creates a rule.
#[utoipa::path(
    post,
    path = "/api/v1/rules",
    operation_id = "createRule",
    tag = tags::RULES,
    description = "Creates a rule matching on the bank description, optionally narrowed by \
                   account and amount, that sets a category, an assignee or both. It runs on \
                   transactions arriving from now on unless created with `enabled: false`; \
                   nothing already held changes until it is run by hand. Needs `write`, and \
                   write on the rule's account, or on every account for a rule without one. \
                   With an `Idempotency-Key`, a retry of the same request within 24 hours \
                   returns the first response instead of creating again.",
    security(("bearer" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header,
         description = "1 to 128 visible ASCII characters naming this create, unique per \
                        token (or session) for 24 hours."),
    ),
    request_body = RuleInput,
    responses(
        (status = 201, description = "The rule, created now or by an earlier request with the \
                                      same `Idempotency-Key`.", body = RuleView),
        (status = 409, description = "A request with this `Idempotency-Key` is still in \
                                      progress.",
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
    TypedJsonBytes { value, bytes }: TypedJsonBytes<RuleInput>,
) -> Result<Response, Problem> {
    let request = CreateRequest {
        key,
        method: &method,
        uri: &uri,
        body: &bytes,
    };
    let account = value.account;
    let authorize = async || Ok(state.ledger.authorize_rule_input(&actor, account).await?);
    idempotency::create(&state, &actor, request, authorize, async |slot| {
        state.ledger.create_rule(&actor, value, slot).await
    })
    .await
}

/// One rule.
#[utoipa::path(
    get,
    path = "/api/v1/rules/{rule_id}",
    operation_id = "getRule",
    tag = tags::RULES,
    description = "One rule. Needs `read`; one the token may not see answers like one that \
                   doesn't exist.",
    security(("bearer" = [])),
    params(RulePath),
    responses(
        (status = 200, description = "The rule.", body = RuleView),
        CommonProblems,
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
) -> Result<Json<RuleView>, Problem> {
    Ok(Json(state.ledger.get_rule(&actor, path.rule_id).await?))
}

/// Changes a rule.
#[utoipa::path(
    patch,
    path = "/api/v1/rules/{rule_id}",
    operation_id = "updateRule",
    tag = tags::RULES,
    description = "Changes the fields given. What the rule already did stays as it is. Needs \
                   `write`, and write on the accounts the rule is for, before and after.",
    security(("bearer" = [])),
    params(RulePath),
    request_body = RulePatch,
    responses(
        (status = 200, description = "The rule as it now is.", body = RuleView),
        CommonProblems,
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
    TypedJson(changes): TypedJson<RulePatch>,
) -> Result<Json<RuleView>, Problem> {
    Ok(Json(
        state
            .ledger
            .update_rule(&actor, path.rule_id, changes)
            .await?,
    ))
}

/// Deletes a rule.
#[utoipa::path(
    delete,
    path = "/api/v1/rules/{rule_id}",
    operation_id = "deleteRule",
    tag = tags::RULES,
    description = "Deletes a rule. What it did stays in place, still naming it. Needs `write`, \
                   and write on the accounts the rule is for.",
    security(("bearer" = [])),
    params(RulePath),
    responses(
        (status = 204, description = "The rule is gone."),
        CommonProblems,
    ),
)]
pub(super) async fn delete(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
) -> Result<StatusCode, Problem> {
    state.ledger.delete_rule(&actor, path.rule_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a rule would change.
#[utoipa::path(
    get,
    path = "/api/v1/rules/{rule_id}/dry-run",
    operation_id = "dryRunRule",
    tag = tags::RULES,
    description = "What running the rule by hand would change now, and what it would leave \
                   alone and why. Writes nothing. Needs `read`, and covers only transactions \
                   the token can read: nothing in it counts any other.",
    security(("bearer" = [])),
    params(RulePath, DryRunQuery),
    responses(
        (status = 200, description = "What it would change.", body = RuleDryRun),
        CommonProblems,
    ),
)]
pub(super) async fn dry_run(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
    TypedQuery(query): TypedQuery<DryRunQuery>,
) -> Result<Json<RuleDryRun>, Problem> {
    Ok(Json(
        state
            .ledger
            .dry_run_rule(&actor, path.rule_id, query.limit)
            .await?,
    ))
}

/// Runs a rule by hand.
#[utoipa::path(
    post,
    path = "/api/v1/rules/{rule_id}/apply",
    operation_id = "applyRule",
    tag = tags::RULES,
    description = "Runs the rule over the transactions already held, filling only what nobody \
                   has set, in short batches, each audited with its count. It never marks \
                   anything reviewed. Safe to repeat: what is filled isn't filled again, and if \
                   the call is cut short what it did stays, so calling again carries on. Needs \
                   `write`, and write on the accounts the rule is for.",
    security(("bearer" = [])),
    params(RulePath),
    responses(
        (status = 200, description = "What it changed.", body = RuleApplied),
        CommonProblems,
    ),
)]
pub(super) async fn apply(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
) -> Result<Json<RuleApplied>, Problem> {
    Ok(Json(state.ledger.apply_rule(&actor, path.rule_id).await?))
}

/// Takes back what a rule filled.
#[utoipa::path(
    post,
    path = "/api/v1/rules/{rule_id}/undo",
    operation_id = "undoRule",
    tag = tags::RULES,
    description = "Takes back what the rule filled and nobody has changed since, in short \
                   batches, each audited with its count. Each field becomes empty and open \
                   again, as if no rule had run, so a corrected rule can fill it, which clearing \
                   it by hand would prevent. A reviewed transaction is left for a person. List \
                   what it would touch with `GET /api/v1/transactions?rule=`. It doesn't change \
                   the rule: turn it off or correct it first, or it files new arrivals again. \
                   Needs `write`, and write on the rule's account, or on every account for a rule \
                   without one. A rule since deleted needs write on every account.",
    security(("bearer" = [])),
    params(RulePath),
    responses(
        (status = 200, description = "What it took back.", body = RuleUndone),
        CommonProblems,
    ),
)]
pub(super) async fn undo(
    State(state): State<AppState>,
    ApiActor(actor): ApiActor,
    TypedPath(path): TypedPath<RulePath>,
) -> Result<Json<RuleUndone>, Problem> {
    Ok(Json(state.ledger.undo_rule(&actor, path.rule_id).await?))
}
