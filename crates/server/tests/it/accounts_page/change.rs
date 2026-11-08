//! Changing an account from its page, and closing and reopening it.

use axum::http::{StatusCode, header};
use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::AccountId;

use super::{CHILD, Reply, World, by_member, input, post, world};

const MARKER: &str = "MarkerZq";

impl World {
    /// Posts the change form of `id` as the admin, from a page showing the account as it is,
    /// with what it shows replaced by `fields`.
    async fn change(&mut self, id: AccountId, fields: &[(&str, &str)]) -> Reply {
        let account = self.account(&id.to_string()).await;
        let text = |key: &str| match &account[key] {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Bool(flag) => flag.to_string(),
            _ => String::new(),
        };
        let shown = [
            ("shown_kind", text("kind")),
            ("shown_belongs_to", text("belongs_to")),
            ("shown_chosen", text("belongs_to_overridden")),
            ("shown_beneficiary", text("beneficiary")),
            ("shown_liquidity", text("liquidity")),
            ("shown_liquidity_chosen", text("liquidity_overridden")),
            ("shown_net_worth", text("include_in_net_worth")),
            ("shown_forecast", text("include_in_forecast")),
        ];
        // What the form starts with: the account as it is, boxes ticked as its flags.
        let mut sent: Vec<(&str, String)> = shown.to_vec();
        sent.push(("name", text("name")));
        sent.push(("kind", text("kind")));
        sent.push(("beneficiary", text("beneficiary")));
        for (flag, field) in [
            ("include_in_net_worth", "net_worth"),
            ("include_in_forecast", "forecast"),
        ] {
            if account[flag] == true {
                sent.push((field, "yes".to_owned()));
            }
        }
        for (name, value) in fields {
            sent.retain(|(sent_name, _)| sent_name != name);
            if !value.is_empty() || *name == "beneficiary" || *name == "name" {
                sent.push((name, (*value).to_owned()));
            }
        }
        let pairs: Vec<(&str, &str)> = sent
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        post(&mut self.admin, &format!("/accounts/{id}"), &pairs).await
    }
}

fn assert_back_to(reply: &Reply, id: AccountId) {
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(reply.header(header::LOCATION), format!("/accounts/{id}"));
}

#[tokio::test]
async fn name_kind_and_class_change() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let reply = world
        .change(
            id,
            &[
                ("name", "Joint"),
                ("kind", "savings"),
                ("liquidity", "invested"),
            ],
        )
        .await;
    assert_back_to(&reply, id);
    let account = world.account(&id.to_string()).await;
    assert_eq!(account["name"], "Joint");
    assert_eq!(account["kind"], "savings");
    assert_eq!(account["liquidity"], "invested");
    assert_eq!(account["liquidity_overridden"], true);
    assert_eq!(account["currency"], "USD");
    let audited = world.audited(&id.to_string(), "account.update").await;
    assert_eq!(audited.len(), 1);
    assert!(by_member(&audited[0], &world.alice));

    let unchanged = world.change(id, &[]).await;
    assert_back_to(&unchanged, id);
    assert_eq!(
        world.audited(&id.to_string(), "account.update").await.len(),
        1,
        "the form as shown asks for nothing"
    );
}

#[tokio::test]
async fn saying_whose_money_it_is_and_back() {
    let mut world = world().await;
    let id = world
        .holding(input("Shares", AccountKind::Brokerage, "USD"))
        .await;
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(
        page.body
            .contains("Net worth shows it apart, in a bucket beside the total")
    );

    let away = world
        .change(
            id,
            &[("belongs_to", "someone_else"), ("beneficiary", CHILD)],
        )
        .await;
    assert_back_to(&away, id);
    let account = world.account(&id.to_string()).await;
    assert_eq!(account["belongs_to"], "someone_else");
    assert_eq!(account["beneficiary"], CHILD);
    assert_eq!(account["include_in_net_worth"], false);
    assert_eq!(account["include_in_forecast"], false);
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(page.body.contains("Held for Child A"));
    assert!(!page.body.contains("Count it in net worth"));

    let back = world.change(id, &[("belongs_to", "household")]).await;
    assert_back_to(&back, id);
    let account = world.account(&id.to_string()).await;
    assert_eq!(account["belongs_to"], "household");
    assert!(
        account["beneficiary"].is_null(),
        "the name goes, as the form says"
    );
    assert_eq!(account["include_in_net_worth"], true);
}

#[tokio::test]
async fn unticking_a_box_leaves_the_account_out() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let reply = world.change(id, &[("net_worth", "")]).await;
    assert_back_to(&reply, id);
    let account = world.account(&id.to_string()).await;
    assert_eq!(account["include_in_net_worth"], false);
    assert_eq!(account["include_in_forecast"], true);
    let page = world.admin.get("/accounts").await;
    assert!(page.body.contains("Left out of net worth"));
}

#[tokio::test]
async fn a_form_older_than_the_account_changes_nothing() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let csrf = world.admin.get("/accounts").await.csrf();
    let reply = world
        .admin
        .post_form(
            &format!("/accounts/{id}"),
            &[
                ("csrf", &csrf),
                ("shown_kind", "savings"),
                ("shown_belongs_to", "household"),
                ("shown_chosen", "false"),
                ("shown_beneficiary", ""),
                ("shown_liquidity", "cash"),
                ("shown_liquidity_chosen", "false"),
                ("shown_net_worth", "true"),
                ("shown_forecast", "true"),
                ("name", "Renamed"),
                ("kind", "checking"),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body.contains("changed after the page opened"));
    assert_eq!(world.account(&id.to_string()).await["name"], "Everyday");
}

#[tokio::test]
async fn a_refusal_says_why_without_echoing() {
    let mut world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let long = MARKER.repeat(20);
    for (fields, words) in [
        (
            vec![("name", long.as_str())],
            "The name takes 1 to 100 characters",
        ),
        (vec![("name", "")], "The name takes 1 to 100 characters"),
        (
            vec![("beneficiary", MARKER)],
            "Held for is only for money that isn't the household's",
        ),
        (vec![("kind", MARKER)], "Choose one of the account kinds"),
        (vec![("liquidity", MARKER)], "Choose how spendable it is"),
        (vec![("forecast", MARKER)], "isn't an answer"),
    ] {
        let reply = world.change(id, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{words}");
        assert!(reply.body.contains(words), "{words}");
        assert!(!reply.body.contains(MARKER), "never echoes: {words}");
    }
    assert_eq!(
        world.audited(&id.to_string(), "account.update").await.len(),
        0
    );

    let forged = world.change(id, &[("currency", "EUR")]).await;
    assert_eq!(forged.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(world.account(&id.to_string()).await["currency"], "USD");
}

#[tokio::test]
async fn closing_and_reopening() {
    let mut world = world().await;
    let id = world
        .holding(input("Old card", AccountKind::CreditCard, "USD"))
        .await;
    let path = format!("/accounts/{id}/closing");
    let closed = post(&mut world.admin, &path, &[("closed", "true")]).await;
    assert_back_to(&closed, id);
    assert_eq!(world.account(&id.to_string()).await["closed"], true);
    let list = world.admin.get("/accounts").await;
    assert!(list.body.contains("Closed (1)"));
    let page = world.admin.get(&format!("/accounts/{id}")).await;
    assert!(page.body.contains("Reopen account"));

    let reopened = post(&mut world.admin, &path, &[("closed", "false")]).await;
    assert_back_to(&reopened, id);
    assert_eq!(world.account(&id.to_string()).await["closed"], false);
    let audited = world.audited(&id.to_string(), "account.update").await;
    assert_eq!(audited.len(), 2);

    let odd = post(&mut world.admin, &path, &[("closed", MARKER)]).await;
    assert_eq!(odd.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!odd.body.contains(MARKER));
}

#[tokio::test]
async fn an_unknown_account_is_not_found() {
    let mut world = world().await;
    let unknown = AccountId::new();
    let changed = post(
        &mut world.admin,
        &format!("/accounts/{unknown}"),
        &[("name", "A")],
    )
    .await;
    assert_eq!(changed.status, StatusCode::NOT_FOUND);
    let closed = post(
        &mut world.admin,
        &format!("/accounts/{unknown}/closing"),
        &[("closed", "true")],
    )
    .await;
    assert_eq!(closed.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_member_changes_as_themselves() {
    let world = world().await;
    let id = world
        .holding(input("Everyday", AccountKind::Checking, "USD"))
        .await;
    let mut bob = world.member().await;
    let reply = post(
        &mut bob,
        &format!("/accounts/{id}/closing"),
        &[("closed", "true")],
    )
    .await;
    assert_back_to(&reply, id);
    let audited = world.audited(&id.to_string(), "account.update").await;
    assert_eq!(audited.len(), 1);
    assert!(!by_member(&audited[0], &world.alice));
}
