//! Saying whose money an account is from the page: the bind form's two fields, and a form on a
//! bound row, both through the same services as the API (spec §8.11 B2, B4, B5).

use axum::http::StatusCode;
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::identity::Role;
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_server::ledger::accounts::AccountView;
use coffer_server::testkit::browser::{Browser, Reply};
use coffer_server::testkit::{captured_log_text, log_marker};
use coffer_store::{AuditEvent, AuditFilter, PageRequest};
use serde_json::Value;

use super::bind::create_path;
use super::{World, assert_inline_free, world};

mod row;

const CHILD: &str = "Child A";
const MARKED_NAME: &str = "Marked Name Zq";
const FOLLOW_THE_KIND: &str = "Follow the kind";

fn row_path(id: AccountId) -> String {
    format!("/connections/accounts/{id}/belonging")
}

/// Creates a provider account named `name` and posts the bind form for it with `fields`.
async fn post_bind(
    world: &mut World,
    name: &str,
    fields: &[(&str, &str)],
) -> (ProviderAccountId, Reply) {
    let connection = world.seeded(world.alice).await;
    let record = world.report(connection, name, "USD", None).await;
    let csrf = world.admin.get("/connections").await.csrf();
    let mut sent = vec![("csrf", csrf.as_str()), ("name", name)];
    sent.extend_from_slice(fields);
    let reply = world.admin.post_form(&create_path(record.id), &sent).await;
    (record.id, reply)
}

/// Binds a new provider account named `name` with `fields`, and returns the new row's id.
async fn bound(world: &mut World, name: &str, fields: &[(&str, &str)]) -> AccountId {
    let (provider, reply) = post_bind(world, name, fields).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    world.fed(provider).await.unwrap()
}

impl World {
    /// What the provider's row is bound to.
    async fn fed(&self, id: ProviderAccountId) -> Option<AccountId> {
        let mut tx = self.server.store.begin().await.unwrap();
        tx.get_provider_account(id).await.unwrap().account
    }

    async fn view(&self, id: AccountId) -> AccountView {
        self.server
            .ledger
            .get_account(&self.alice, id)
            .await
            .unwrap()
    }

    async fn updates(&self, id: AccountId) -> Vec<AuditEvent> {
        self.server
            .store
            .list_audit(
                &AuditFilter {
                    entity_type: Some("account".into()),
                    entity_id: Some(id.to_string()),
                    ..AuditFilter::default()
                },
                &PageRequest::default(),
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .filter(|event| event.action == "account.update")
            .collect()
    }

    /// Posts the row's form with `fields` as the admin.
    async fn post_row(&mut self, id: AccountId, fields: &[(&str, &str)]) -> Reply {
        let csrf = self.admin.get("/connections").await.csrf();
        let mut sent = vec![("csrf", csrf.as_str())];
        sent.extend_from_slice(fields);
        self.admin.post_form(&row_path(id), &sent).await
    }

    async fn member(&self) -> Browser {
        let invite = self
            .server
            .identity
            .create_invite(&self.alice, Role::Member, None)
            .await
            .unwrap();
        let mut bob = Browser::new(&self.server);
        let mut key = self.server.authenticator();
        let done = bob.enroll(&mut key, &invite, "bob", "Bob").await;
        assert_eq!(done.status, StatusCode::OK);
        bob
    }
}

fn changes(event: &AuditEvent) -> Value {
    serde_json::to_value(&event.changes).unwrap()
}

fn assert_counted(view: &AccountView, counted: bool) {
    assert_eq!(view.include_in_net_worth, counted);
    assert_eq!(view.include_in_forecast, counted);
}

// --- the bind form ---------------------------------------------------------------------------

#[tokio::test]
async fn binding_a_529_from_the_page() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::SomeoneElse);
    assert!(!view.belongs_to_overridden);
    assert_counted(&view, false);
}

#[tokio::test]
async fn binding_with_a_name() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    assert_eq!(world.view(fund).await.beneficiary.as_deref(), Some(CHILD));
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains("Held for Child A"));
}

#[tokio::test]
async fn binding_a_brokerage_for_a_child() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Shares",
        &[("kind", "brokerage"), ("belongs_to", "someone_else")],
    )
    .await;
    let view = world.view(fund).await;
    assert_eq!(view.kind, AccountKind::Brokerage);
    assert_eq!(view.belongs_to, BelongsTo::SomeoneElse);
    assert!(view.belongs_to_overridden);
    assert_counted(&view, false);
}

#[tokio::test]
async fn saying_it_is_the_household_s_counts_it() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("belongs_to", "household")],
    )
    .await;
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::Household);
    assert_counted(&view, true);
}

#[tokio::test]
async fn an_unknown_belongs_to_is_a_notice_and_changes_nothing() {
    let mut world = world().await;
    let (provider, reply) = post_bind(
        &mut world,
        "Plan",
        &[("kind", "education"), ("belongs_to", "the-neighbours")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(!reply.body.contains("the-neighbours"));
    assert_eq!(world.fed(provider).await, None);
}

#[tokio::test]
async fn a_blank_name_is_none() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", "   ")],
    )
    .await;
    assert_eq!(world.view(fund).await.beneficiary, None);
}

#[tokio::test]
async fn a_too_long_name_is_a_notice_without_echo() {
    let mut world = world().await;
    let long = "Marked".repeat(11);
    let (provider, reply) = post_bind(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", &long)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(!reply.body.contains("MarkedMarked"));
    assert_eq!(world.fed(provider).await, None);
}

#[tokio::test]
async fn a_name_on_the_household_s_money_is_a_notice_without_echo() {
    let mut world = world().await;
    let (provider, reply) = post_bind(
        &mut world,
        "Everyday",
        &[("kind", "checking"), ("beneficiary", MARKED_NAME)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(!reply.body.contains(MARKED_NAME));
    assert_eq!(world.fed(provider).await, None);
}

#[tokio::test]
async fn forged_form_fails_whole() {
    let mut world = world().await;
    let (provider, reply) = post_bind(
        &mut world,
        "Plan",
        &[
            ("kind", "education"),
            ("beneficiary", MARKED_NAME),
            ("include_in_net_worth", "true"),
        ],
    )
    .await;
    assert!(reply.status.is_client_error());
    assert!(!reply.body.contains(MARKED_NAME));
    assert_eq!(world.fed(provider).await, None);

    let fund = bound(&mut world, "Other", &[("kind", "education")]).await;
    let refused = world
        .post_row(
            fund,
            &[("kind", "education"), ("include_in_net_worth", "true")],
        )
        .await;
    assert!(refused.status.is_client_error());
    assert_eq!(world.updates(fund).await.len(), 0);
}

#[tokio::test]
async fn an_unknown_provider_account_is_missing_whatever_the_form_says() {
    let mut world = world().await;
    let csrf = world.admin.get("/connections").await.csrf();
    let reply = world
        .admin
        .post_form(
            &create_path(ProviderAccountId::new()),
            &[
                ("csrf", &csrf),
                ("kind", "nonsense"),
                ("belongs_to", "nonsense"),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_bind_form_says_the_kind_decides_unless_told_otherwise() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.report(connection, "Fund", "USD", None).await;
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains(
        "Checking and savings count as cash for planning. Education and custodial accounts are \
         held for someone else, and charitable ones are given away: they count in neither net \
         worth nor planning unless you say they're the household's."
    ));
    let select = page.between("name=\"belongs_to\"", "</select>");
    let order: Vec<usize> = [
        format!("<option value=\"\" selected>{FOLLOW_THE_KIND}</option>"),
        "<option value=\"household\">The household's</option>".to_owned(),
        "<option value=\"someone_else\">Someone else's, held for them</option>".to_owned(),
        "<option value=\"given_away\">Given away</option>".to_owned(),
    ]
    .iter()
    .map(|option| select.find(option.as_str()).unwrap())
    .collect();
    assert!(order.is_sorted());
    assert!(page.body.contains("Whose money is it?"));
    assert!(page.body.contains("Held for (optional)"));
    assert!(page.body.contains("maxlength=\"64\""));
    assert!(page.body.contains("list=\"beneficiaries\""));
}

// --- what the page holds -----------------------------------------------------------------------

#[tokio::test]
async fn the_row_says_whose_money_it_is_unless_it_is_the_household_s() {
    let mut world = world().await;
    bound(&mut world, "Everyday", &[("kind", "checking")]).await;
    let page = world.admin.get("/connections").await;
    for words in ["· Held for", "· Given away"] {
        assert!(!page.body.contains(words), "{words}");
    }
    bound(&mut world, "Plan", &[("kind", "education")]).await;
    bound(
        &mut world,
        "Named",
        &[("kind", "custodial"), ("beneficiary", CHILD)],
    )
    .await;
    bound(&mut world, "Fund", &[("kind", "charitable")]).await;
    bound(
        &mut world,
        "Family",
        &[("kind", "charitable"), ("beneficiary", "Family fund")],
    )
    .await;
    let page = world.admin.get("/connections").await;
    for meta in [
        "· Held for someone else<",
        "· Held for Child A<",
        "· Given away<",
        "· Given away: Family fund<",
    ] {
        assert!(page.body.contains(meta), "{meta}");
    }
}

#[tokio::test]
async fn names_are_escaped() {
    let mut world = world().await;
    let name = "<script>x</script>\"><b>&";
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", name)],
    )
    .await;
    let page = world.admin.get("/connections").await;
    assert_inline_free(&page);
    assert!(!page.body.contains("<script>x"));
    assert!(!page.body.contains("<b>&"));
    assert!(!page.body.contains("\"><b>"));
    assert!(
        page.body
            .contains("Held for &lt;script&gt;x&lt;/script&gt;")
    );
}

#[tokio::test]
async fn the_datalist_holds_only_labels_of_visible_accounts() {
    let mut world = world().await;
    bound(
        &mut world,
        "One",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    bound(
        &mut world,
        "Two",
        &[("kind", "custodial"), ("beneficiary", CHILD)],
    )
    .await;
    bound(
        &mut world,
        "Three",
        &[("kind", "charitable"), ("beneficiary", "Family fund")],
    )
    .await;
    bound(&mut world, "Everyday", &[("kind", "checking")]).await;
    let manual = world.manual_account("By hand", "USD").await;
    world
        .post_row(
            manual,
            &[("belongs_to", "someone_else"), ("beneficiary", "Child B")],
        )
        .await;
    let page = world.admin.get("/connections").await;
    assert_eq!(page.body.matches("<datalist").count(), 1);
    assert_eq!(
        page.between("<datalist id=\"beneficiaries\">", "</datalist>"),
        "<option value=\"Child A\"></option>\
         <option value=\"Child B\"></option>\
         <option value=\"Family fund\"></option>"
    );
}

#[tokio::test]
async fn page_has_the_default_csp_and_no_inline_script_or_style() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let page = world.admin.get("/connections").await;
    assert_inline_free(&page);
    let refused = world
        .post_row(fund, &[("kind", "education"), ("belongs_to", "nonsense")])
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_inline_free(&refused);
}

#[tokio::test]
async fn the_page_shows_no_balance() {
    let mut world = world().await;
    let connection = world.seeded(world.alice).await;
    world.sync_fixture(connection).await;
    let checking = world.provider_account("Checking").await;
    let csrf = world.admin.get("/connections").await.csrf();
    let done = world
        .admin
        .post_form(
            &create_path(checking.id),
            &[("csrf", &csrf), ("name", "Everyday"), ("kind", "checking")],
        )
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains("Kind and whose money it is"));
    for figure in ["2450.17", "2,450.17", "245017"] {
        assert!(!page.body.contains(figure), "{figure}");
    }
}

#[tokio::test]
async fn a_name_never_reaches_the_log() {
    let mut world = world().await;
    let marker = log_marker();
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", MARKED_NAME)],
    )
    .await;
    world
        .post_row(
            fund,
            &[("kind", "education"), ("beneficiary", "Marked Other Zq")],
        )
        .await;
    world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", "household"),
                ("beneficiary", "Marked Third Zq"),
            ],
        )
        .await;
    world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("beneficiary", "Marked\u{0007}Fourth Zq"),
            ],
        )
        .await;
    let log = captured_log_text();
    assert!(
        log.contains(&marker),
        "the capture still holds this test's lines"
    );
    assert!(!log.contains("Marked"));
    assert!(!log.contains("Fourth Zq"));
}
