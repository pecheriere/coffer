//! What a refusal or a list can say: a patch below `write`, where a label shows at each level,
//! and what a problem body repeats.

use axum::http::{Method, StatusCode};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::{AccountLevel, TokenGrant};
use coffer_server::secrets::SecretToken;
use serde_json::{Value, json};

use super::super::net_worth_channels::{HIDDEN_MINOR, RANGE, all};
use super::super::on;
use super::family::{
    HIDDEN_LABEL, HIDDEN_NAME, LABEL, READS, SINCE, assert_same_answers, family, ids_by_first_use,
};

/// What a patch can ask of `belongs_to` and `beneficiary`, including what the state of the
/// account would decide if the state were looked at first.
fn patches() -> Vec<Value> {
    vec![
        json!({}),
        json!({"belongs_to": "household"}),
        json!({"belongs_to": "someone_else"}),
        json!({"belongs_to": "given_away", "beneficiary": "Someone"}),
        json!({"beneficiary": LABEL}),
        json!({"beneficiary": HIDDEN_LABEL}),
        json!({"beneficiary": ""}),
        json!({"beneficiary": "Tab\tinside"}),
        json!({"include_in_net_worth": true}),
        json!({"include_in_forecast": true}),
        json!({"kind": "checking"}),
        json!({"closed": true}),
        json!({"name": HIDDEN_NAME}),
    ]
}

/// A visible account below `write` answers 403 whatever the request or the account's state, and a
/// hidden one answers as a missing one does, every header and byte. Nothing changes.
#[tokio::test]
async fn a_patch_at_balances_is_refused_alike() {
    const NEEDS_WRITE: &str = "this action needs write access to this account";
    let held = family().await;
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let (_, token_at_transactions) = held
        .world
        .token(
            Scope::Read,
            vec![
                on(held.home(), AccountLevel::Transactions),
                on(held.emma(), AccountLevel::Transactions),
            ],
        )
        .await;
    let (_, writer_elsewhere) = held
        .world
        .token(Scope::Write, vec![on(held.home(), AccountLevel::Write)])
        .await;
    let missing = AccountId::new();
    let audited = held.world.audit_count().await;
    let before = held.answers(&held.admin).await;

    let refuse = async |id: AccountId, token: &SecretToken, patch: &Value| {
        let uri = format!("/api/v1/accounts/{id}");
        let (status, body) = held
            .world
            .send(Method::PATCH, &uri, token, Some(patch), None)
            .await;
        (status, ids_by_first_use(&body))
    };
    for patch in patches() {
        for token in [&held.token, &token_at_transactions] {
            for visible in [held.home(), held.emma()] {
                let (status, body) = refuse(visible, token, &patch).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{patch}");
                let body: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(body["code"], "forbidden");
                assert_eq!(body["detail"], NEEDS_WRITE);
            }
        }
        for token in [&held.token, &token_at_transactions, &writer_elsewhere] {
            let hidden_answer = refuse(hidden, token, &patch).await;
            assert_eq!(hidden_answer.0, StatusCode::NOT_FOUND, "{patch}");
            assert_eq!(hidden_answer, refuse(missing, token, &patch).await);
        }
    }

    assert_eq!(held.world.audit_count().await, audited, "nothing written");
    assert_same_answers(&before, &held.answers(&held.admin).await);
}

/// One token and exactly where a label shows for it.
struct Viewer {
    grants: Vec<TokenGrant>,
    scope: Scope,
    /// The reads holding the label, by a part of the uri.
    shows_in: &'static [&'static str],
}

/// A hidden account's label in every read a token can make, at every level: only where the
/// account itself shows, or the audit log at `all` at `transactions`.
#[tokio::test]
async fn a_label_lists_nowhere_else() {
    let held = family().await;
    let (home, emma) = (held.home(), held.emma());
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let own_transaction = held.world.transaction(home, 1, "Groceries").await;
    let hidden_transaction = held.world.transaction(hidden, 2, "Quiet tuition").await;
    held.world
        .connection(&[
            ("Household bank", Some(home)),
            ("Noah bank", Some(hidden)),
            ("Unbound bank", None),
        ])
        .await;
    let category = held.world.expense_category().await;
    held.world
        .planned_item("Tuition", Some(hidden), category, 5_000)
        .await;
    let extras = [
        format!("/api/v1/accounts/{hidden}"),
        format!("/api/v1/accounts/{hidden}/balances"),
        format!("/api/v1/transactions?account={hidden}"),
        format!("/api/v1/transactions/{hidden_transaction}"),
        format!("/api/v1/transactions/{own_transaction}"),
    ];
    let account_reads: &[&str] = &["accounts", "net-worth"];
    let with_audit: &[&str] = &["accounts", "net-worth", "audit"];
    let on_hidden = format!("/api/v1/accounts/{hidden}");
    let mut viewers = Vec::new();
    for (level, scope) in [
        (AccountLevel::Balances, Scope::Read),
        (AccountLevel::Transactions, Scope::Read),
        (AccountLevel::Write, Scope::Write),
    ] {
        viewers.push(Viewer {
            grants: vec![on(home, level), on(emma, level)],
            scope,
            shows_in: &[],
        });
        viewers.push(Viewer {
            grants: vec![on(home, level), on(hidden, level)],
            scope,
            shows_in: account_reads,
        });
    }
    viewers.extend([
        Viewer {
            grants: vec![all(AccountLevel::Balances)],
            scope: Scope::Read,
            shows_in: account_reads,
        },
        Viewer {
            grants: vec![all(AccountLevel::Transactions)],
            scope: Scope::Read,
            shows_in: with_audit,
        },
    ]);

    for viewer in viewers {
        let (_, token) = held.world.token(viewer.scope, viewer.grants).await;
        let mut uris: Vec<String> = READS.iter().map(ToString::to_string).collect();
        uris.extend(extras.iter().cloned());
        for uri in &uris {
            let text = held.text(uri, &token).await;
            let is_account_read = uri.starts_with("/api/v1/accounts")
                && (uri == "/api/v1/accounts"
                    || uri.starts_with("/api/v1/accounts?")
                    || *uri == on_hidden);
            let expected = viewer.shows_in.iter().any(|place| match *place {
                "accounts" => is_account_read,
                "net-worth" => uri.contains("/reports/net-worth"),
                _ => uri.contains(place),
            });
            assert_eq!(
                text.contains(HIDDEN_LABEL),
                expected,
                "{uri} for a viewer showing it in {:?}",
                viewer.shows_in
            );
        }
    }
}

/// What a request that must be refused holds: input naming a child, sent where it can't go.
const INPUT: &str = "Amelia Secretname";

/// One request, as `(method, uri, body)`.
type Attempt = (Method, String, Option<Value>);

/// Patches of `id` refused for what they ask, however the account stands.
fn refused_patches(id: AccountId) -> Vec<Attempt> {
    let uri = format!("/api/v1/accounts/{id}");
    [
        json!({"beneficiary": format!("{INPUT}\u{7}")}),
        json!({"beneficiary": format!("{INPUT}{}", "x".repeat(70))}),
        json!({"include_in_net_worth": true}),
        json!({"belongs_to": INPUT}),
        json!({"kind": INPUT}),
        json!({INPUT: true}),
        json!({"beneficiary": {"x": INPUT}}),
    ]
    .into_iter()
    .map(|body| (Method::PATCH, uri.clone(), Some(body)))
    .collect()
}

/// Reads refused for a query or a path naming a label, and a create with one.
fn refused_others() -> Vec<Attempt> {
    let name = INPUT.replace(' ', "%20");
    vec![
        (Method::GET, format!("{RANGE}&interval={name}"), None),
        (
            Method::GET,
            format!("/api/v1/reports/net-worth?beneficiary={name}"),
            None,
        ),
        (
            Method::GET,
            format!("/api/v1/accounts?beneficiary={name}"),
            None,
        ),
        (Method::GET, format!("/api/v1/accounts/{name}"), None),
        (
            Method::POST,
            "/api/v1/accounts".to_owned(),
            Some(
                json!({"name": "x", "kind": "custodial", "currency": "USD", "beneficiary": INPUT}),
            ),
        ),
    ]
}

/// Input that names a label comes back in no refusal, whatever is refused: a label on the
/// household's money, one no account may have, a flag that can't be on, a word where a kind goes.
#[tokio::test]
async fn no_problem_body_names_a_label() {
    let held = family().await;
    let (home, emma) = (held.home(), held.emma());
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let (_, writer) = held
        .world
        .token(
            Scope::Write,
            vec![on(home, AccountLevel::Write), on(emma, AccountLevel::Write)],
        )
        .await;
    let (_, creator) = held
        .world
        .token(Scope::Write, vec![all(AccountLevel::Write)])
        .await;
    let label_it = Some(json!({"beneficiary": INPUT}));
    let account = |id: AccountId| format!("/api/v1/accounts/{id}");
    let mut attempts: Vec<(Attempt, &SecretToken)> = refused_patches(emma)
        .into_iter()
        .chain(refused_others())
        .map(|attempt| (attempt, &writer))
        .collect();
    attempts.extend([
        ((Method::PATCH, account(home), label_it.clone()), &writer),
        ((Method::PATCH, account(hidden), label_it.clone()), &writer),
        ((Method::PATCH, account(emma), label_it), &held.token),
        (
            (
                Method::POST,
                "/api/v1/accounts".to_owned(),
                Some(json!({"name": "x", "kind": "checking", "currency": "USD", "beneficiary": INPUT})),
            ),
            &creator,
        ),
        (
            (
                Method::POST,
                "/api/v1/accounts".to_owned(),
                Some(json!({"name": "x", "kind": INPUT, "currency": "USD"})),
            ),
            &creator,
        ),
    ]);
    for ((method, uri, body), token) in attempts {
        let (status, text) = held
            .world
            .send(method, &uri, token, body.as_ref(), None)
            .await;
        assert!(status.is_client_error(), "{uri} {body:?}: {status}");
        for label in [INPUT, LABEL, HIDDEN_LABEL, "Amelia", "Secretname"] {
            assert!(!text.contains(label), "{uri} {body:?}: {text}");
        }
    }
}

/// A bucket too large for the token to sum is refused in fixed words, and only for a token that
/// sees it.
#[tokio::test]
async fn a_bucket_too_large_is_refused_naming_no_label() {
    let held = family().await;
    let emma = held.emma();
    let second = held.world.held_for(LABEL, i64::MAX).await;
    held.world.balance_on(emma, SINCE, i64::MAX).await;
    let (_, both) = held
        .world
        .token(
            Scope::Read,
            vec![
                on(emma, AccountLevel::Balances),
                on(second, AccountLevel::Balances),
            ],
        )
        .await;
    let (status, _, text) = held.world.get_whole(RANGE, &both).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        !text.contains(LABEL) && !text.contains(HIDDEN_NAME),
        "{text}"
    );
    assert_eq!(
        held.world.get_whole(RANGE, &held.token).await.0,
        StatusCode::OK
    );
}
