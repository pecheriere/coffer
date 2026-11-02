//! A kind suggested from a provider's name, on the bind form: preselected for a member to see
//! and confirm, never applied by itself (spec §8.11 B12).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::identity::{Scope, TokenName};
use coffer_server::http::router;
use coffer_server::testkit::{capture_logs, captured_log_text, log_marker};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::json;
use tower::ServiceExt as _;

use super::bind::create_path;
use super::{World, assert_inline_free, world};

const SENTENCE: &str =
    "Suggested from the account's name, so check it: the kind decides what counts as cash.";
const CHOOSE: &str = "<option value=\"\" selected disabled>Choose a kind</option>";
/// `binding.rs`'s words for a bind with no kind, as a notice.
const KIND_REQUIRED: &str = "Kind is required: the provider gives no kind for this account.";

/// The values of a select's options marked selected.
fn selected(select: &str) -> Vec<String> {
    select
        .split("<option ")
        .filter(|option| option.split('>').next().unwrap().contains(" selected"))
        .map(|option| {
            option
                .split("value=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// A world with the provider account `name` reported with `hint`, and the select that page draws.
async fn reported(
    name: &str,
    hint: Option<AccountKind>,
) -> (World, coffer_store::ProviderAccountRecord) {
    let world = world().await;
    let connection = world.seeded(world.alice).await;
    let record = world.report(connection, name, "USD", hint).await;
    (world, record)
}

#[tokio::test]
async fn the_form_preselects_a_suggestion_and_says_so() {
    let (mut world, _) = reported("My 529 Plan", None).await;

    let page = world.admin.get("/connections").await;

    assert_eq!(page.status, StatusCode::OK);
    assert_inline_free(&page);
    let select = page.between("name=\"kind\"", "</select>");
    assert_eq!(selected(&select), ["education"]);
    assert!(!select.contains("Choose a kind"));
    assert!(page.body.contains(SENTENCE), "the guess is called a guess");
    let select_at = page.body.find("name=\"kind\"").unwrap();
    let sentence_at = page.body.find(SENTENCE).unwrap();
    assert!(
        select_at < sentence_at,
        "the sentence is under what it's about"
    );
    let described = page.between("aria-describedby=\"kind-guess-", " ");
    assert!(
        page.body
            .contains(&format!("id=\"kind-guess-{described}\"")),
        "the select is described by the sentence, for who can't see it beneath"
    );
}

#[tokio::test]
async fn a_provider_hint_wins_over_the_name() {
    let (mut world, _) = reported("529 Plan", Some(AccountKind::Checking)).await;

    let page = world.admin.get("/connections").await;

    let select = page.between("name=\"kind\"", "</select>");
    assert_eq!(selected(&select), ["checking"]);
    assert!(
        !page.body.contains(SENTENCE),
        "the provider said so: it isn't a guess from the name"
    );
    assert!(!page.body.contains("kind-guess-"));
}

#[tokio::test]
async fn no_suggestion_keeps_choose_a_kind() {
    for name in ["Family Fund", "Visa Checking", "Roth 529"] {
        let (mut world, _) = reported(name, None).await;

        let page = world.admin.get("/connections").await;

        let select = page.between("name=\"kind\"", "</select>");
        assert!(select.contains(CHOOSE), "{name}");
        assert_eq!(selected(&select), [""], "{name}");
        assert!(!page.body.contains(SENTENCE), "{name}");
    }
}

#[tokio::test]
async fn an_empty_kind_is_not_filled_from_the_name() {
    let (mut world, record) = reported("My 529 Plan", None).await;
    let page = world.admin.get("/connections").await;
    let csrf = page.csrf();

    let posts: [&[(&str, &str)]; 2] = [
        &[("csrf", &csrf), ("name", "My 529 Plan"), ("kind", "")],
        &[("csrf", &csrf), ("name", "My 529 Plan")],
    ];
    for fields in posts {
        let reply = world.admin.post_form(&create_path(record.id), fields).await;

        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(reply.body.contains(KIND_REQUIRED));
        assert!(world.accounts().await.is_empty(), "no account");
        assert_eq!(world.provider_account("My 529 Plan").await.account, None);
    }
}

#[tokio::test]
async fn the_kind_posted_is_the_kind_kept() {
    let (mut world, record) = reported("My 529 Plan", None).await;
    let page = world.admin.get("/connections").await;
    assert_eq!(
        selected(&page.between("name=\"kind\"", "</select>")),
        ["education"]
    );

    world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "My 529 Plan"),
                ("kind", "brokerage"),
            ],
        )
        .await;

    let accounts = world.accounts().await;
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].kind, AccountKind::Brokerage);
    assert_eq!(accounts[0].belongs_to, BelongsTo::Household);
    assert!(accounts[0].include_in_net_worth);
}

#[tokio::test]
async fn a_suggested_529_binds_out_of_net_worth() {
    let (mut world, record) = reported("College Savings 529", None).await;
    let page = world.admin.get("/connections").await;
    let select = page.between("name=\"kind\"", "</select>");
    let shown = selected(&select);

    // What a browser sends for the form as drawn, and as the member left it.
    world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "College"),
                ("kind", &shown[0]),
                ("belongs_to", ""),
                ("beneficiary", ""),
            ],
        )
        .await;

    let accounts = world.accounts().await;
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].kind, AccountKind::Education);
    assert_eq!(accounts[0].belongs_to, BelongsTo::SomeoneElse);
    assert!(!accounts[0].include_in_net_worth);
    assert!(!accounts[0].include_in_forecast);
}

/// A provider's name is untrusted: however it reads, it stays text in the page, and all it can
/// do to the form is pick one of the same fifteen kinds.
#[tokio::test]
async fn a_hostile_name_stays_text_and_moves_only_the_preselection() {
    let hostile = "\"><script>alert(1)</script><option value=\"x\" selected> 529";
    let (mut world, _) = reported(hostile, None).await;

    let page = world.admin.get("/connections").await;

    assert_inline_free(&page);
    assert!(!page.body.contains("<script>alert"));
    assert!(!page.body.contains("<option value=\"x\""));
    assert!(!page.body.contains("value=\"x\" selected>"));
    let select = page.between("name=\"kind\"", "</select>");
    assert_eq!(selected(&select), ["education"]);
    assert_eq!(select.matches("<option ").count(), AccountKind::ALL.len());
}

#[tokio::test]
async fn a_suggestion_is_neither_logged_nor_audited() {
    capture_logs();
    let name_marker = "NAME-MARKER-39";
    let name = format!("{name_marker} 529 Plan");
    let (mut world, record) = reported(&name, None).await;
    let start = log_marker();

    let (_, token) = world
        .server
        .agent_tokens
        .create(
            &world.alice,
            TokenName::parse("claude").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let listing = router(world.server.app_state())
        .oneshot(
            Request::builder()
                .uri("/api/v1/provider-accounts")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token.expose_secret()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains(name_marker), "the page did show it");
    world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "Plan"),
                ("kind", "brokerage"),
            ],
        )
        .await;
    assert_eq!(world.accounts().await.len(), 1, "the bind happened");

    let log = captured_log_text();
    assert!(log.contains(&start), "the capture still holds this test");
    assert!(!log.contains(name_marker), "the name isn't logged");
    assert!(!log.contains("suggest"), "the suggestion isn't logged");

    let audit = world
        .server
        .store
        .list_audit(&AuditFilter::default(), &PageRequest::default())
        .await
        .unwrap()
        .items;
    let created = audit
        .iter()
        .find(|event| event.action == "account.create")
        .unwrap();
    let changes = serde_json::to_value(&created.changes).unwrap();
    assert_eq!(
        changes["kind"]["after"],
        json!("brokerage"),
        "what was chosen is what's recorded"
    );
    for event in &audit {
        let changes = serde_json::to_string(&event.changes).unwrap();
        assert!(!changes.contains("suggest"), "{}", event.action);
        assert!(!changes.contains(name_marker), "{}", event.action);
    }
}
