//! Creating a manual account from the list, with every field the API takes.

use axum::http::{StatusCode, header};

use super::{CHILD, World, by_member, post, world};

const MARKER: &str = "MarkerZq";

/// Posts the new account form as the admin.
async fn create(world: &mut World, fields: &[(&str, &str)]) -> super::Reply {
    post(&mut world.admin, "/accounts", fields).await
}

/// The id of the account a create answered with, from where it sends the browser.
fn created_id(reply: &super::Reply) -> String {
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    reply
        .header(header::LOCATION)
        .strip_prefix("/accounts/")
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn every_field_reaches_the_account() {
    let mut world = world().await;
    let reply = create(
        &mut world,
        &[
            ("name", "Shares for A"),
            ("kind", "brokerage"),
            ("currency", "USD"),
            ("liquidity", "cash"),
            ("belongs_to", "someone_else"),
            ("beneficiary", CHILD),
            ("leave_out_net_worth", "yes"),
            ("opening_amount", "1234.56"),
            ("opening_on", "2026-01-02"),
        ],
    )
    .await;
    let id = created_id(&reply);
    let account = world.account(&id).await;
    assert_eq!(account["name"], "Shares for A");
    assert_eq!(account["kind"], "brokerage");
    assert_eq!(account["currency"], "USD");
    assert_eq!(account["liquidity"], "cash");
    assert_eq!(account["liquidity_overridden"], true);
    assert_eq!(account["belongs_to"], "someone_else");
    assert_eq!(account["belongs_to_overridden"], true);
    assert_eq!(account["beneficiary"], CHILD);
    assert_eq!(account["include_in_net_worth"], false);
    assert_eq!(account["include_in_forecast"], false);
    assert_eq!(account["source"], "manual");
    assert_eq!(account["latest_balance"]["on"], "2026-01-02");
    assert_eq!(account["latest_balance"]["balance"]["minor_units"], 123_456);

    let audited = world.audited(&id, "account.create").await;
    assert_eq!(audited.len(), 1);
    assert!(by_member(&audited[0], &world.alice));
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(page.body.contains("<h1>Shares for A</h1>"));
}

#[tokio::test]
async fn left_to_follow_the_kind_it_does() {
    let mut world = world().await;
    let plan = created_id(
        &create(
            &mut world,
            &[("name", "Plan"), ("kind", "education"), ("currency", "USD")],
        )
        .await,
    );
    let plan = world.account(&plan).await;
    assert_eq!(plan["belongs_to"], "someone_else");
    assert_eq!(plan["belongs_to_overridden"], false);
    assert_eq!(plan["liquidity_overridden"], false);
    assert_eq!(plan["include_in_net_worth"], false);
    assert!(
        plan["latest_balance"].is_null(),
        "an empty amount opens nothing"
    );

    let checking = created_id(
        &create(
            &mut world,
            &[
                ("name", "Everyday"),
                ("kind", "checking"),
                ("currency", "JPY"),
                ("leave_out_forecast", "yes"),
                ("opening_amount", "-5000"),
                ("opening_on", "2026-01-02"),
            ],
        )
        .await,
    );
    let checking = world.account(&checking).await;
    assert_eq!(checking["belongs_to"], "household");
    assert_eq!(checking["liquidity"], "cash");
    assert_eq!(checking["include_in_net_worth"], true);
    assert_eq!(checking["include_in_forecast"], false);
    assert_eq!(
        checking["latest_balance"]["balance"]["minor_units"], -5000,
        "read exactly in yen"
    );
}

#[tokio::test]
async fn a_refusal_says_why_in_fixed_words_and_creates_nothing() {
    let mut world = world().await;
    let base = [("kind", "checking"), ("currency", "USD")];
    let long = MARKER.repeat(20);
    let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (vec![("name", &long)], "The name takes 1 to 100 characters"),
        (
            vec![("name", "A"), ("beneficiary", MARKER)],
            "Held for is only for money that isn't the household's",
        ),
        (
            vec![
                ("name", "A"),
                ("belongs_to", "given_away"),
                ("beneficiary", "Zq\u{7}Marker"),
            ],
            "Held for takes a name or a purpose",
        ),
        (
            vec![
                ("name", "A"),
                ("opening_amount", "12.345"),
                ("opening_on", "2026-01-02"),
            ],
            "more decimal places than its currency allows",
        ),
        (
            vec![
                ("name", "A"),
                ("opening_amount", "12MarkerZq"),
                ("opening_on", "2026-01-02"),
            ],
            "A balance is digits",
        ),
        (
            vec![
                ("name", "A"),
                ("opening_amount", "1"),
                ("opening_on", "2026-02-30"),
            ],
            "The opening balance needs a real day",
        ),
        (
            vec![("name", "A"), ("liquidity", MARKER)],
            "Choose how spendable it is",
        ),
        (
            vec![("name", "A"), ("belongs_to", MARKER)],
            "Choose one of the answers",
        ),
        (
            vec![("name", "A"), ("leave_out_forecast", MARKER)],
            "isn't an answer",
        ),
    ];
    for (fields, words) in cases {
        let mut sent: Vec<(&str, &str)> = base.to_vec();
        sent.extend(fields);
        let reply = create(&mut world, &sent).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{words}");
        assert!(reply.body.contains(words), "{words}");
        assert!(!reply.body.contains(MARKER), "never echoes: {words}");
    }
    for (field, value) in [("kind", MARKER), ("currency", "ZZZ")] {
        let mut sent = vec![("name", "A"), ("kind", "checking"), ("currency", "USD")];
        sent.retain(|(name, _)| *name != field);
        sent.push((field, value));
        let reply = create(&mut world, &sent).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert!(!reply.body.contains(value), "{field}");
    }
    assert_eq!(world.every_account().await.len(), 0);
}

#[tokio::test]
async fn a_forged_or_unsigned_form_creates_nothing() {
    let mut world = world().await;
    let forged = create(
        &mut world,
        &[
            ("name", "A"),
            ("kind", "checking"),
            ("currency", "USD"),
            ("include_in_net_worth", "true"),
        ],
    )
    .await;
    assert_eq!(forged.status, StatusCode::UNPROCESSABLE_ENTITY);
    let unsigned = world
        .admin
        .post_form(
            "/accounts",
            &[("name", "A"), ("kind", "checking"), ("currency", "USD")],
        )
        .await;
    assert_eq!(unsigned.status, StatusCode::FORBIDDEN);
    assert_eq!(world.every_account().await.len(), 0);
}

#[tokio::test]
async fn a_member_creates_as_themselves() {
    let mut world = world().await;
    let mut bob = world.member().await;
    let reply = post(
        &mut bob,
        "/accounts",
        &[("name", "Bob's"), ("kind", "savings"), ("currency", "EUR")],
    )
    .await;
    let id = created_id(&reply);
    assert_eq!(world.account(&id).await["name"], "Bob's");
    let audited = world.audited(&id, "account.create").await;
    assert_eq!(audited.len(), 1);
    assert!(!by_member(&audited[0], &world.alice));
}
