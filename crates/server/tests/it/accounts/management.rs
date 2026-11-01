//! Creating and changing accounts, their balances and their audit trail.

use super::{audit_actions, call, create, token, world};
use axum::http::{Method, StatusCode};
use coffer_domain::audit::{AuditActor, AuditValue};
use coffer_domain::identity::Scope;
use serde_json::{Value, json};

#[tokio::test]
async fn create_manual_account_defaults_liquidity() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    let created = create(
        &world.server,
        &write,
        json!({"name": "Visa", "kind": "credit_card", "currency": "USD"}),
    )
    .await;

    assert_eq!(created["name"], "Visa");
    assert_eq!(created["kind"], "credit_card");
    assert_eq!(created["currency"], "USD");
    assert_eq!(created["liquidity"], "short_term_liability");
    assert_eq!(created["liquidity_overridden"], false);
    assert_eq!(created["source"], "manual");
    assert_eq!(created["include_in_net_worth"], true);
    assert_eq!(created["include_in_forecast"], true);
    assert_eq!(created["closed"], false);
    assert!(created["latest_balance"].is_null());

    let id = created["id"].as_str().unwrap();
    let (status, fetched) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}"),
        &write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, created);
}

#[tokio::test]
async fn chosen_liquidity_is_kept_as_an_override() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Emergency fund", "kind": "brokerage", "currency": "EUR",
               "liquidity": "cash", "include_in_forecast": false}),
    )
    .await;
    assert_eq!(created["liquidity"], "cash");
    assert_eq!(created["liquidity_overridden"], true);
    assert_eq!(created["include_in_forecast"], false);
    let id = created["id"].as_str().unwrap();

    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"kind": "retirement"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["kind"], "retirement");
    assert_eq!(
        patched["liquidity"], "cash",
        "an override survives a kind change"
    );
}

#[tokio::test]
async fn kind_change_redefaults_liquidity() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Card", "kind": "credit_card", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"kind": "savings"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["liquidity"], "cash");
    assert_eq!(patched["liquidity_overridden"], false);
}

#[tokio::test]
async fn opening_balance_recorded_as_manual_snapshot() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;

    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "JPY",
               "opening_balance": {"on": "2026-10-01", "minor_units": -12345}}),
    )
    .await;

    let expected = json!({
        "on": "2026-10-01",
        "balance": {"minor_units": -12345, "currency": "JPY"},
        "source": "manual",
    });
    assert_eq!(created["latest_balance"], expected);
    let id = created["id"].as_str().unwrap();
    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances"),
        &write,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page, json!({"items": [expected], "next": null}));
}

#[tokio::test]
async fn manual_balance_last_write_wins_and_lists_newest_first() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (_, read) = token(&world, Scope::Read).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    for (on, minor) in [
        ("2026-10-01", 100),
        ("2026-10-02", 200),
        ("2026-10-01", 150),
    ] {
        let (status, body) = call(
            &world.server,
            Method::PUT,
            &format!("/api/v1/accounts/{id}/balances/{on}"),
            &write,
            Some(json!({"minor_units": minor})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["balance"]["minor_units"], minor);
        assert_eq!(body["source"], "manual");
    }

    let (status, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?limit=1"),
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["on"], "2026-10-02");
    let next = page["next"].as_str().unwrap();
    let (_, page) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?limit=1&after={next}"),
        &read,
        None,
    )
    .await;
    assert_eq!(page["items"][0]["on"], "2026-10-01");
    assert_eq!(page["items"][0]["balance"]["minor_units"], 150);
    assert!(page["next"].is_null());

    let (status, body) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}/balances?after=SECRETVALUE"),
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_cursor");
    assert!(
        !body.to_string().contains("SECRET"),
        "the response must not contain the secret"
    );

    let (status, body) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/2026-10-03"),
        &read,
        Some(json!({"minor_units": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["detail"],
        "this action needs write access to this account"
    );
}

#[tokio::test]
async fn closed_accounts_are_listed_only_on_request() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let (_, read) = token(&world, Scope::Read).await;
    let kept = create(
        &world.server,
        &write,
        json!({"name": "Kept", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let closed = create(
        &world.server,
        &write,
        json!({"name": "Old", "kind": "savings", "currency": "USD"}),
    )
    .await;
    let closed_id = closed["id"].as_str().unwrap();
    let (status, patched) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{closed_id}"),
        &write,
        Some(json!({"closed": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["closed"], true);

    let names = |body: &Value| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let (status, listed) = call(&world.server, Method::GET, "/api/v1/accounts", &read, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&listed), [kept["name"].as_str().unwrap()]);
    let (_, listed) = call(
        &world.server,
        Method::GET,
        "/api/v1/accounts?include_closed=true",
        &read,
        None,
    )
    .await;
    assert_eq!(names(&listed), ["Kept", "Old"]);
}

#[tokio::test]
async fn currency_is_immutable() {
    let world = world().await;
    let (_, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD"}),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"currency": "EUR"})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid");
    let (_, fetched) = call(
        &world.server,
        Method::GET,
        &format!("/api/v1/accounts/{id}"),
        &write,
        None,
    )
    .await;
    assert_eq!(fetched["currency"], "USD");
}

#[tokio::test]
async fn account_changes_audited_with_agent() {
    let world = world().await;
    let (agent, write) = token(&world, Scope::Write).await;
    let created = create(
        &world.server,
        &write,
        json!({"name": "Checking", "kind": "checking", "currency": "USD",
               "opening_balance": {"on": "2026-10-01", "minor_units": 500}}),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"name": "Joint checking", "include_in_net_worth": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &world.server,
        Method::PUT,
        &format!("/api/v1/accounts/{id}/balances/2026-10-01"),
        &write,
        Some(json!({"minor_units": 750})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let by_agent = AuditActor::Member {
        member: world.alice.member,
        agent: Some(agent),
    };
    let created_events = audit_actions(&world.server, "account.create").await;
    assert_eq!(created_events.len(), 1);
    let event = &created_events[0];
    assert_eq!(event.actor, by_agent);
    assert_eq!(event.entity_id, id);
    let fields: Vec<_> = event.changes.iter().map(|(field, _)| field).collect();
    for field in [
        "name",
        "kind",
        "currency",
        "liquidity",
        "opening_balance_on",
    ] {
        assert!(
            fields.contains(&field),
            "a created account must record every set field"
        );
    }

    let updates = audit_actions(&world.server, "account.update").await;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].actor, by_agent);
    let changes: Vec<_> = updates[0].changes.iter().collect();
    assert_eq!(changes.len(), 2);
    let name = changes
        .iter()
        .find(|(field, _)| *field == "name")
        .unwrap()
        .1;
    assert_eq!(name.before, AuditValue::from("Checking"));
    assert_eq!(name.after, AuditValue::from("Joint checking"));

    let balances = audit_actions(&world.server, "balance.set").await;
    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0].actor, by_agent);
    let minor = balances[0]
        .changes
        .iter()
        .find(|(field, _)| *field == "balance_minor")
        .unwrap()
        .1;
    assert_eq!(minor.before, AuditValue::Int(500));
    assert_eq!(minor.after, AuditValue::Int(750));

    // A patch that changes nothing records nothing.
    let (status, _) = call(
        &world.server,
        Method::PATCH,
        &format!("/api/v1/accounts/{id}"),
        &write,
        Some(json!({"name": "Joint checking"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        audit_actions(&world.server, "account.update").await.len(),
        1
    );
}
