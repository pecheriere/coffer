//! A kind suggested from a provider's name, on the bind form: preselected for a member to see
//! and confirm, never applied by itself (spec §8.11 B12).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::identity::{Scope, TokenName};
use coffer_domain::ids::ProviderAccountId;
use coffer_server::http::router;
use coffer_server::testkit::browser::Reply;
use coffer_server::testkit::{capture_logs, captured_log_text, log_marker};
use coffer_store::{AuditFilter, PageRequest};
use secrecy::ExposeSecret as _;
use serde_json::json;
use tower::ServiceExt as _;

use super::bind::{bind_path, create_path};
use super::{World, assert_inline_free, world};

const SENTENCE: &str =
    "Suggested from the account's name, so check it: the kind decides what counts as cash.";
/// The sentence under a guess that leaves the account out of net worth, naming the kind by the
/// label the select shows, so it stays true when the member changes the select.
fn sentence_leaving_out(label: &str) -> String {
    format!(
        "Suggested from the account's name, so check it: the kind decides what counts as cash, \
         and {label} leaves the account out of net worth unless you say it's the household's."
    )
}
/// Both sentences start so.
const GUESS: &str = "Suggested from the account's name";
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
    assert!(
        page.body
            .contains(&sentence_leaving_out("Education savings (529)")),
        "the guess is called a guess"
    );
    let select_at = page.body.find("name=\"kind\"").unwrap();
    let sentence_at = page.body.find(GUESS).unwrap();
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

/// Which kinds leave an account out of net worth by themselves is a stake worth saying, and only
/// those say it.
#[tokio::test]
async fn the_sentence_says_when_the_kind_leaves_it_out_of_net_worth() {
    let world = world().await;
    let connection = world.seeded(world.alice).await;
    let leaving_out = [
        ("My 529 Plan", "Education savings (529)"),
        ("UTMA Account", "Custodial (UTMA, UGMA)"),
        ("Donor Advised Fund", "Charitable (donor-advised fund)"),
    ];
    let staying_in = [
        "Everyday Checking",
        "High Yield Savings",
        "Visa Signature",
        "Car Loan",
        "Home Mortgage",
        "Brokerage",
        "Roth IRA",
        "Stock Plan",
    ];
    let mut rows = Vec::new();
    for (name, label) in leaving_out {
        let id = world.report(connection, name, "USD", None).await.id;
        rows.push((name, id, sentence_leaving_out(label)));
    }
    for name in staying_in {
        let id = world.report(connection, name, "USD", None).await.id;
        rows.push((name, id, SENTENCE.to_owned()));
    }
    let mut world = world;

    let page = world.admin.get("/connections").await;

    for (name, id, expected) in rows {
        let sentence = page.between(&format!("id=\"kind-guess-{id}\">"), "</span>");
        assert_eq!(sentence, expected, "{name}");
    }
}

#[tokio::test]
async fn a_provider_hint_wins_over_the_name() {
    let (mut world, _) = reported("529 Plan", Some(AccountKind::Checking)).await;

    let page = world.admin.get("/connections").await;

    let select = page.between("name=\"kind\"", "</select>");
    assert_eq!(selected(&select), ["checking"]);
    assert!(
        !page.body.contains(GUESS),
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
        assert!(!page.body.contains(GUESS), "{name}");
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

/// The select of the row `id` on `page`.
fn select_of(page: &Reply, id: ProviderAccountId) -> String {
    page.between(&format!("id=\"kind-{id}\""), "</select>")
}

/// A member corrects the guess, and is refused for something else. The form must not put the guess
/// they rejected back in front of them, where following the notice would store it.
#[tokio::test]
async fn a_refusal_does_not_put_back_the_guess_the_member_rejected() {
    let (mut world, record) = reported("College Savings", None).await;
    let other = world
        .report(record.connection, "My 529 Plan", "USD", None)
        .await;
    let page = world.admin.get("/connections").await;
    assert_eq!(selected(&select_of(&page, record.id)), ["education"]);
    let csrf = page.csrf();

    let refused = world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &csrf),
                ("name", "College Savings"),
                ("kind", "brokerage"),
                ("belongs_to", ""),
                ("beneficiary", "Child A"),
            ],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused.body.contains("role=\"alert\""));
    let select = select_of(&refused, record.id);
    assert!(select.contains(CHOOSE), "a fresh choice to make");
    assert_eq!(selected(&select), [""]);
    assert!(
        !refused
            .body
            .contains(&format!("id=\"kind-guess-{}\"", record.id)),
        "nothing to check: nothing is preselected"
    );
    assert_eq!(
        selected(&select_of(&refused, other.id)),
        ["education"],
        "the other rows keep theirs"
    );
    assert!(
        refused
            .body
            .contains(&format!("id=\"kind-guess-{}\"", other.id))
    );

    // What a browser would send of the form as redrawn, had it not stopped at `required`: a
    // disabled option sends nothing.
    let again = world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &refused.csrf()),
                ("name", "College Savings"),
                ("belongs_to", ""),
                ("beneficiary", "Child A"),
            ],
        )
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(again.body.contains(KIND_REQUIRED));
    assert!(world.accounts().await.is_empty(), "the guess isn't stored");
    assert_eq!(
        selected(&select_of(&again, record.id)),
        [""],
        "and a second refusal doesn't bring it back"
    );

    world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &again.csrf()),
                ("name", "College Savings"),
                ("kind", "brokerage"),
                ("belongs_to", ""),
                ("beneficiary", ""),
            ],
        )
        .await;
    let accounts = world.accounts().await;
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].kind, AccountKind::Brokerage);
    assert_eq!(accounts[0].belongs_to, BelongsTo::Household);
    assert!(accounts[0].include_in_net_worth);
}

/// However a bind form of the row is refused, the guess stays out of the redrawn row.
#[tokio::test]
async fn every_refusal_of_a_row_clears_its_guess() {
    let (mut world, record) = reported("College Savings", None).await;
    let euros = world.manual_account("Euros", "EUR").await.to_string();
    let page = world.admin.get("/connections").await;
    let csrf = page.csrf();
    let create = create_path(record.id);
    let link = bind_path(record.id);
    let refusals: [(&str, &[(&str, &str)]); 5] = [
        (
            &create,
            &[("csrf", &csrf), ("name", "Plan"), ("kind", "not-a-kind")],
        ),
        (
            &create,
            &[
                ("csrf", &csrf),
                ("name", "Plan"),
                ("kind", "education"),
                ("belongs_to", "not-an-answer"),
            ],
        ),
        (
            &create,
            &[("csrf", &csrf), ("name", "\u{7}"), ("kind", "education")],
        ),
        (&link, &[("csrf", &csrf), ("account", "not-an-id")]),
        (&link, &[("csrf", &csrf), ("account", &euros)]),
    ];
    for (path, fields) in refusals {
        let refused = world.admin.post_form(path, fields).await;

        assert!(refused.status.is_client_error(), "{fields:?}");
        assert_eq!(
            selected(&select_of(&refused, record.id)),
            [""],
            "{fields:?}"
        );
        assert!(!refused.body.contains(GUESS), "{fields:?}");
    }
    let fresh = world.admin.get("/connections").await;
    assert_eq!(
        selected(&select_of(&fresh, record.id)),
        ["education"],
        "a refusal is no record: the page opened anew suggests again, and says so"
    );
    assert!(fresh.body.contains(GUESS));
}

/// Only the first row to set up starts open, so a refusal of another opens it: its choice is gone,
/// and the member sees that.
#[tokio::test]
async fn the_row_refused_is_open() {
    let (mut world, first) = reported("College Savings", None).await;
    let second = world
        .report(first.connection, "My 529 Plan", "USD", None)
        .await;
    let drawn = |open: &str, id: ProviderAccountId| {
        format!(
            "<details class=\"item-detail\"{open}><summary>Bind to an account</summary><form \
             method=\"post\" action=\"{}\"",
            create_path(id)
        )
    };
    let page = world.admin.get("/connections").await;
    let folded = [first.id, second.id]
        .into_iter()
        .find(|id| page.body.contains(&drawn("", *id)))
        .unwrap();

    let refused = world
        .admin
        .post_form(
            &create_path(folded),
            &[("csrf", &page.csrf()), ("name", "Plan"), ("kind", "nope")],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused.body.contains(&drawn(" open", folded)));
}

/// Whatever refuses a create, the row comes back open: the first waiting row, which starts open,
/// and a later one, which starts folded.
#[tokio::test]
async fn every_refusal_leaves_its_row_open() {
    let (mut world, first) = reported("Plan one", None).await;
    let later = world
        .report(first.connection, "Plan two", "USD", None)
        .await;
    let opening = |id: ProviderAccountId| {
        format!(
            "<summary>Bind to an account</summary><form method=\"post\" action=\"{}\"",
            create_path(id)
        )
    };
    let is_open = |body: &str, id: ProviderAccountId| {
        body.contains(&format!(
            "<details class=\"item-detail\" open>{}",
            opening(id)
        ))
    };
    let page = world.admin.get("/connections").await;
    assert_eq!(
        [first.id, later.id].map(|id| is_open(&page.body, id)),
        [true, false],
        "the order the page draws them in"
    );
    let csrf = page.csrf();
    let too_long = "x".repeat(65);
    let refusals: [&[(&str, &str)]; 5] = [
        &[("kind", "nope")],
        &[("kind", "education"), ("belongs_to", "nope")],
        &[],
        &[("kind", "checking"), ("beneficiary", "Child A")],
        &[("kind", "education"), ("beneficiary", &too_long)],
    ];

    for id in [first.id, later.id] {
        for fields in refusals {
            let mut form = vec![("csrf", csrf.as_str()), ("name", "Plan")];
            form.extend_from_slice(fields);

            let refused = world.admin.post_form(&create_path(id), &form).await;

            assert_eq!(
                refused.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{fields:?}"
            );
            assert!(is_open(&refused.body, id), "{fields:?}");
        }
    }
}

/// A provider's own hint isn't a guess the member could have rejected by name: a refusal keeps it.
#[tokio::test]
async fn a_refusal_keeps_a_provider_hint() {
    let (mut world, record) = reported("College Savings", Some(AccountKind::Checking)).await;
    let page = world.admin.get("/connections").await;

    let refused = world
        .admin
        .post_form(
            &create_path(record.id),
            &[
                ("csrf", &page.csrf()),
                ("name", "Plan"),
                ("kind", "brokerage"),
                ("beneficiary", "Child A"),
            ],
        )
        .await;

    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(selected(&select_of(&refused, record.id)), ["checking"]);
    assert!(!refused.body.contains(GUESS));
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
