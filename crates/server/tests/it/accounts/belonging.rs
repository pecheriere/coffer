//! Whose money an account is, through the real router: the one rule for a create and a patch
//! (spec §8.11 B2 to B5). `binds` covers a provider account's way in, `access` what a restricted
//! token gets, and `leaks` where a beneficiary must never turn up.

mod access;
mod binds;
mod leaks;

use super::{World, account_count, audit_actions, call, token, world};
use axum::http::{Method, StatusCode};
use coffer_domain::accounts::{AccountKind, BelongsTo, Beneficiary, LiquidityClass};
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_server::secrets::SecretToken;
use coffer_store::AccountRecord;
use serde_json::{Value, json};

const COUNTED_REFUSAL: &str =
    "an account that isn't the household's can't be counted: set belongs_to to household first";
const LABEL_ON_HOUSEHOLD: &str = "only an account that isn't the household's has a beneficiary";
const LABEL_INVALID: &str = "beneficiary must be 1 to 64 characters with something visible and \
                             no control, format or separator characters";

async fn admin() -> (World, SecretToken) {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    (world, write)
}

/// What the body answers, for a body the server must accept.
async fn post(world: &World, write: &SecretToken, body: Value) -> Value {
    let (status, created) = call(
        &world.server,
        Method::POST,
        "/api/v1/accounts",
        write,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

async fn make(world: &World, write: &SecretToken, kind: &str) -> Value {
    post(
        world,
        write,
        json!({"name": "Fund", "kind": kind, "currency": "USD"}),
    )
    .await
}

/// A 529, held for `Child A` by default.
async fn held_529(world: &World, write: &SecretToken) -> Value {
    post(
        world,
        write,
        json!({"name": "Fund", "kind": "education", "currency": "USD", "beneficiary": "Child A"}),
    )
    .await
}

fn id_of(answer: &Value) -> AccountId {
    answer["id"].as_str().unwrap().parse().unwrap()
}

fn uri_of(answer: &Value) -> String {
    format!("/api/v1/accounts/{}", answer["id"].as_str().unwrap())
}

async fn patch(
    world: &World,
    write: &SecretToken,
    target: &Value,
    body: Value,
) -> (StatusCode, Value) {
    call(
        &world.server,
        Method::PATCH,
        &uri_of(target),
        write,
        Some(body),
    )
    .await
}

async fn stored(world: &World, answer: &Value) -> AccountRecord {
    world.server.store.get_account(id_of(answer)).await.unwrap()
}

/// `(include_in_net_worth, include_in_forecast)`, from an answer.
fn flags(answer: &Value) -> (bool, bool) {
    (
        answer["include_in_net_worth"].as_bool().unwrap(),
        answer["include_in_forecast"].as_bool().unwrap(),
    )
}

fn stored_label(record: &AccountRecord) -> Option<&str> {
    record.beneficiary.as_ref().map(Beneficiary::as_str)
}

async fn assert_nothing_audited(world: &World, action: &str) {
    let entries = audit_actions(&world.server, action).await;
    assert_eq!(entries.len(), 0, "nothing was audited");
}

fn stored_flags(record: &AccountRecord) -> (bool, bool) {
    (record.include_in_net_worth, record.include_in_forecast)
}

/// The fields of the one `action` entry that `target` has, each as `{before, after}`.
async fn audited(world: &World, write: &SecretToken, target: &Value, action: &str) -> Value {
    let entity = target["id"].as_str().unwrap();
    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/audit?entity_type=account&entity_id={entity}"),
        write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let found: Vec<&Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["action"] == action)
        .collect();
    assert_eq!(found.len(), 1, "one entry for {action}");
    found[0]["changes"].clone()
}

/// Asserts a creating refusal: its status and its fixed words, and nothing created.
async fn assert_create_refused(world: &World, write: &SecretToken, body: Value, words: &str) {
    let (status, problem) = call(
        &world.server,
        Method::POST,
        "/api/v1/accounts",
        write,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "invalid");
    assert_eq!(problem["detail"], words);
    assert_eq!(account_count(&world.server).await, 0, "nothing was created");
    assert_nothing_audited(world, "account.create").await;
}

async fn assert_defaults_out(kind: &str, belongs_to: BelongsTo, words: &str) {
    let (world, write) = admin().await;
    let created = make(&world, &write, kind).await;
    assert_eq!(created["belongs_to"], words);
    assert_eq!(created["liquidity"], "invested");
    assert_eq!(created["beneficiary"], Value::Null);
    assert_eq!(flags(&created), (false, false));

    let record = stored(&world, &created).await;
    assert_eq!(record.belongs_to, belongs_to);
    assert!(!record.belongs_to_overridden, "it took the default");
    assert_eq!(record.liquidity, LiquidityClass::Invested);
    assert_eq!(stored_flags(&record), (false, false));
}

#[tokio::test]
async fn creating_a_529_defaults_it_out() {
    assert_defaults_out("education", BelongsTo::SomeoneElse, "someone_else").await;
}

#[tokio::test]
async fn creating_a_custodial_defaults_it_out() {
    assert_defaults_out("custodial", BelongsTo::SomeoneElse, "someone_else").await;
}

#[tokio::test]
async fn creating_a_charitable_defaults_it_out() {
    assert_defaults_out("charitable", BelongsTo::GivenAway, "given_away").await;
}

#[tokio::test]
async fn creating_a_checking_stays_counted() {
    let (world, write) = admin().await;
    let created = make(&world, &write, "checking").await;
    assert_eq!(created["belongs_to"], "household");
    assert_eq!(flags(&created), (true, true));
    assert!(!stored(&world, &created).await.belongs_to_overridden);
}

#[tokio::test]
async fn asking_to_count_it_is_422() {
    for flag in ["include_in_net_worth", "include_in_forecast"] {
        let (world, write) = admin().await;
        let mut body = json!({"name": "Fund", "kind": "education", "currency": "USD"});
        body[flag] = json!(true);
        assert_create_refused(&world, &write, body, COUNTED_REFUSAL).await;
    }
}

#[tokio::test]
async fn asking_to_count_a_brokerage_held_for_someone_is_422() {
    let (world, write) = admin().await;
    let body = json!({"name": "Fund", "kind": "brokerage", "currency": "USD",
                      "belongs_to": "someone_else", "include_in_forecast": true});
    assert_create_refused(&world, &write, body, COUNTED_REFUSAL).await;
}

#[tokio::test]
async fn a_brokerage_held_for_a_child() {
    let (world, write) = admin().await;
    let created = post(
        &world,
        &write,
        json!({"name": "Fund", "kind": "brokerage", "currency": "USD",
               "belongs_to": "someone_else", "beneficiary": "Child A"}),
    )
    .await;
    assert_eq!(created["kind"], "brokerage");
    assert_eq!(created["belongs_to"], "someone_else");
    assert_eq!(created["beneficiary"], "Child A");
    assert_eq!(flags(&created), (false, false));

    let record = stored(&world, &created).await;
    assert_eq!(record.kind, AccountKind::Brokerage);
    assert!(record.belongs_to_overridden, "it was chosen");
    assert_eq!(stored_label(&record), Some("Child A"));
}

#[tokio::test]
async fn a_529_counted_as_the_household_s() {
    let (world, write) = admin().await;
    let created = post(
        &world,
        &write,
        json!({"name": "Fund", "kind": "education", "currency": "USD",
               "belongs_to": "household"}),
    )
    .await;
    assert_eq!(created["belongs_to"], "household");
    assert_eq!(created["liquidity"], "invested");
    assert_eq!(flags(&created), (true, true));
    assert!(stored(&world, &created).await.belongs_to_overridden);
}

#[tokio::test]
async fn a_beneficiary_on_a_new_household_account_is_422() {
    let (world, write) = admin().await;
    let body = json!({"name": "Fund", "kind": "checking", "currency": "USD",
                      "beneficiary": "Child A"});
    assert_create_refused(&world, &write, body, LABEL_ON_HOUSEHOLD).await;
}

#[tokio::test]
async fn an_empty_beneficiary_on_a_create_is_the_same_as_absent() {
    let (world, write) = admin().await;
    for (kind, empty) in [("education", ""), ("checking", ""), ("education", "   ")] {
        let created = post(
            &world,
            &write,
            json!({"name": "Fund", "kind": kind, "currency": "USD", "beneficiary": empty}),
        )
        .await;
        assert_eq!(created["beneficiary"], Value::Null);
        assert_eq!(stored(&world, &created).await.beneficiary, None);
    }
}

#[tokio::test]
async fn a_bad_beneficiary_on_a_create_is_422() {
    let (world, write) = admin().await;
    let body = json!({"name": "Fund", "kind": "education", "currency": "USD",
                      "beneficiary": "Child\u{200B}A"});
    assert_create_refused(&world, &write, body, LABEL_INVALID).await;
}

#[tokio::test]
async fn a_create_audits_whose_money_it_is() {
    let (world, write) = admin().await;
    let created = held_529(&world, &write).await;
    let changes = audited(&world, &write, &created, "account.create").await;
    for (field, after) in [
        ("belongs_to", json!("someone_else")),
        ("belongs_to_overridden", json!(false)),
        ("beneficiary", json!("Child A")),
        ("include_in_net_worth", json!(false)),
        ("include_in_forecast", json!(false)),
    ] {
        assert_eq!(changes[field]["after"], after, "{field}");
    }
}

#[tokio::test]
async fn patching_the_kind_moves_it_out_and_audits() {
    let (world, write) = admin().await;
    let target = make(&world, &write, "other_asset").await;
    let (status, moved) = patch(&world, &write, &target, json!({"kind": "education"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["belongs_to"], "someone_else");
    assert_eq!(moved["liquidity"], "invested");
    assert_eq!(flags(&moved), (false, false));

    let record = stored(&world, &target).await;
    assert_eq!(record.belongs_to, BelongsTo::SomeoneElse);
    assert_eq!(stored_flags(&record), (false, false));

    assert_eq!(
        audited(&world, &write, &target, "account.update").await,
        json!({
            "kind": {"before": "other_asset", "after": "education"},
            "liquidity": {"before": "illiquid", "after": "invested"},
            "belongs_to": {"before": "household", "after": "someone_else"},
            "include_in_net_worth": {"before": true, "after": false},
            "include_in_forecast": {"before": true, "after": false},
        })
    );
}

#[tokio::test]
async fn patching_the_kind_back_brings_it_in() {
    let (world, write) = admin().await;
    let target = make(&world, &write, "education").await;
    let (status, moved) = patch(&world, &write, &target, json!({"kind": "brokerage"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["belongs_to"], "household");
    assert_eq!(flags(&moved), (true, true));
}

#[tokio::test]
async fn a_chosen_household_stays_over_a_patched_kind() {
    let (world, write) = admin().await;
    let target = make(&world, &write, "checking").await;
    let (status, chosen) = patch(&world, &write, &target, json!({"belongs_to": "household"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        stored(&world, &target).await.belongs_to_overridden,
        "its own default, asked for, is chosen"
    );
    assert_eq!(chosen["belongs_to"], "household");
    assert_eq!(
        audited(&world, &write, &target, "account.update").await,
        json!({"belongs_to_overridden": {"before": false, "after": true}})
    );

    let (status, moved) = patch(&world, &write, &target, json!({"kind": "education"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["belongs_to"], "household");
    assert_eq!(flags(&moved), (true, true));
}

#[tokio::test]
async fn a_household_flag_stays_over_a_patched_kind() {
    let (world, write) = admin().await;
    let target = post(
        &world,
        &write,
        json!({"name": "Fund", "kind": "checking", "currency": "USD",
               "include_in_net_worth": false}),
    )
    .await;
    let (status, moved) = patch(&world, &write, &target, json!({"kind": "savings"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(flags(&moved), (false, true));
}

#[tokio::test]
async fn patching_a_flag_on_while_held_is_422() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    for flag in ["include_in_net_worth", "include_in_forecast"] {
        let mut body = json!({"name": "Renamed"});
        body[flag] = json!(true);
        let (status, problem) = patch(&world, &write, &target, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["detail"], COUNTED_REFUSAL);
    }
    let record = stored(&world, &target).await;
    assert_eq!(record.name.as_str(), "Fund", "nothing else changed");
    assert_eq!(stored_flags(&record), (false, false));
    assert_nothing_audited(&world, "account.update").await;
}

#[tokio::test]
async fn patching_a_flag_off_while_held_writes_nothing() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    let (status, same) = patch(
        &world,
        &write,
        &target,
        json!({"include_in_net_worth": false, "include_in_forecast": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(flags(&same), (false, false));
    assert_nothing_audited(&world, "account.update").await;
}

#[tokio::test]
async fn patching_to_the_household_and_a_flag_together_works() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    let (status, back) = patch(
        &world,
        &write,
        &target,
        json!({"belongs_to": "household", "include_in_net_worth": true,
               "include_in_forecast": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(back["belongs_to"], "household");
    assert_eq!(flags(&back), (true, false));
    assert_eq!(
        audited(&world, &write, &target, "account.update").await,
        json!({
            "belongs_to": {"before": "someone_else", "after": "household"},
            "belongs_to_overridden": {"before": false, "after": true},
            "beneficiary": {"before": "Child A", "after": null},
            "include_in_net_worth": {"before": false, "after": true},
        })
    );
}

#[tokio::test]
async fn moving_away_turns_both_flags_off_in_the_table() {
    let (world, write) = admin().await;
    let target = make(&world, &write, "brokerage").await;
    let (status, moved) = patch(
        &world,
        &write,
        &target,
        json!({"belongs_to": "given_away", "beneficiary": "Family fund"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["beneficiary"], "Family fund");
    let record = stored(&world, &target).await;
    assert_eq!(record.belongs_to, BelongsTo::GivenAway);
    assert_eq!(stored_flags(&record), (false, false));
}

#[tokio::test]
async fn an_empty_beneficiary_removes_it() {
    for empty in ["", "   "] {
        let (world, write) = admin().await;
        let target = held_529(&world, &write).await;
        let (status, removed) = patch(&world, &write, &target, json!({"beneficiary": empty})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(removed["beneficiary"], Value::Null);
        assert_eq!(stored(&world, &target).await.beneficiary, None);
        assert_eq!(
            audited(&world, &write, &target, "account.update").await,
            json!({"beneficiary": {"before": "Child A", "after": null}})
        );
    }
}

#[tokio::test]
async fn a_whitespace_only_one_is_422_and_keeps_it() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    for blank in ["\u{00A0}", "\u{3000}", "\t"] {
        let (status, problem) = patch(&world, &write, &target, json!({"beneficiary": blank})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["detail"], LABEL_INVALID);
    }
    let record = stored(&world, &target).await;
    assert_eq!(stored_label(&record), Some("Child A"));
}

#[tokio::test]
async fn a_whitespace_only_one_on_a_create_is_422() {
    let (world, write) = admin().await;
    let body = json!({"name": "Fund", "kind": "education", "currency": "USD",
                      "beneficiary": "\u{00A0}"});
    assert_create_refused(&world, &write, body, LABEL_INVALID).await;
}

#[tokio::test]
async fn the_answer_says_whether_it_was_chosen() {
    let (world, write) = admin().await;
    let defaulted = make(&world, &write, "education").await;
    assert_eq!(defaulted["belongs_to_overridden"], false);

    let (_, followed) = patch(&world, &write, &defaulted, json!({"kind": "brokerage"})).await;
    assert_eq!(followed["belongs_to_overridden"], false, "it followed");

    let (_, own) = patch(
        &world,
        &write,
        &defaulted,
        json!({"belongs_to": "household"}),
    )
    .await;
    assert_eq!(own["belongs_to_overridden"], true, "its own default, asked");

    let chosen = post(
        &world,
        &write,
        json!({"name": "Fund", "kind": "brokerage", "currency": "USD",
               "belongs_to": "someone_else"}),
    )
    .await;
    assert_eq!(chosen["belongs_to_overridden"], true);
}

#[tokio::test]
async fn an_absent_one_keeps_it() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    for body in [json!({"name": "Renamed"}), json!({"beneficiary": null})] {
        let (status, kept) = patch(&world, &write, &target, body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kept["beneficiary"], "Child A");
    }
    assert_eq!(
        audited(&world, &write, &target, "account.update").await,
        json!({"name": {"before": "Fund", "after": "Renamed"}})
    );
}

#[tokio::test]
async fn a_bad_one_is_422_without_echo() {
    let (world, write) = admin().await;
    let target = held_529(&world, &write).await;
    for bad in [
        "Child B\u{0007}".to_owned(),
        "Child B\u{202E}".to_owned(),
        format!("Child B{}", "x".repeat(64)),
        "\u{200B}".to_owned(),
    ] {
        let (status, problem) = patch(&world, &write, &target, json!({"beneficiary": bad})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        assert_eq!(problem["detail"], LABEL_INVALID);
        assert!(!problem.to_string().contains("Child B"), "no echo");
    }
    let record = stored(&world, &target).await;
    assert_eq!(stored_label(&record), Some("Child A"));
}

#[tokio::test]
async fn a_beneficiary_patched_on_the_household_is_422() {
    let (world, write) = admin().await;
    let target = make(&world, &write, "checking").await;
    let (status, problem) = patch(&world, &write, &target, json!({"beneficiary": "Child A"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["detail"], LABEL_ON_HOUSEHOLD);
    assert_eq!(stored(&world, &target).await.beneficiary, None);
}

#[tokio::test]
async fn a_patch_that_changes_nothing_writes_nothing() {
    let (world, write) = admin().await;
    let target = post(
        &world,
        &write,
        json!({"name": "Fund", "kind": "brokerage", "currency": "USD",
               "belongs_to": "someone_else", "beneficiary": "Child A"}),
    )
    .await;
    let (status, same) = patch(
        &world,
        &write,
        &target,
        json!({"kind": "brokerage", "belongs_to": "someone_else", "beneficiary": " Child A ",
               "include_in_net_worth": false, "include_in_forecast": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same, target);
    assert_nothing_audited(&world, "account.update").await;
}
