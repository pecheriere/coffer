//! The form on a bound row: what it says will happen, and that it does.

use axum::http::StatusCode;
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::audit::AuditActor;
use coffer_domain::ids::AccountId;
use coffer_server::testkit::browser::Browser;
use serde_json::json;

use super::super::world;
use super::{CHILD, FOLLOW_THE_KIND, MARKED_NAME, assert_counted, bound, changes, row_path};

const LEAVE_IT_FOLLOWING: &str = "Leave it, following the kind";
const LEAVE_IT_CHOSEN: &str = "Leave it as you chose";
const COUNTS_IT_AGAIN: &str = "it counts in both again, even one you had turned off";
const COUNTS_IT: &str =
    "Saying it's the household's counts it in net worth and planning, over its whole history";
const TAKES_IT_OUT: &str = "Saying it's someone else's or given away takes it out of net worth and planning, over its whole history.";
const STAYS: &str =
    "You chose this, so it stays whatever the kind. It can't go back to following the kind.";
const HOUSEHOLD_FOLLOWS: &str =
    "changing to an education, custodial or charitable kind takes it out of net worth and planning";
const HELD_FOLLOWS: &str = "changing to any kind but education, custodial or charitable counts it";

#[tokio::test]
async fn csrf_is_required() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let path = row_path(fund);
    let missing = world
        .admin
        .post_form(&path, &[("belongs_to", "household")])
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let wrong = world
        .admin
        .post_form(&path, &[("csrf", "AAAA"), ("belongs_to", "household")])
        .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(
        world.view(fund).await.belongs_to,
        BelongsTo::SomeoneElse,
        "nothing changed"
    );
}

#[tokio::test]
async fn the_row_form_changes_kind_and_belonging() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "brokerage")]).await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "custodial"),
                ("belongs_to", "given_away"),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);

    let view = world.view(fund).await;
    assert_eq!(view.kind, AccountKind::Custodial);
    assert_eq!(view.belongs_to, BelongsTo::GivenAway);
    assert_eq!(view.beneficiary.as_deref(), Some(CHILD));
    assert_counted(&view, false);

    let updates = world.updates(fund).await;
    assert_eq!(updates.len(), 1);
    let AuditActor::Member { member, agent } = updates[0].actor else {
        panic!("the update names its member");
    };
    assert_eq!(member, world.alice.member);
    assert_eq!(agent, None);
    let audited = changes(&updates[0]);
    assert_eq!(
        audited["kind"],
        json!({"before": "brokerage", "after": "custodial"})
    );
    assert_eq!(
        audited["belongs_to"],
        json!({"before": "household", "after": "given_away"})
    );
    assert_eq!(audited["beneficiary"]["after"], json!(CHILD));
}

#[tokio::test]
async fn the_row_form_removes_a_name() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", ""),
                ("beneficiary", ""),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.beneficiary, None);
    assert_eq!(view.belongs_to, BelongsTo::SomeoneElse);
    let page = world.admin.get("/connections").await;
    assert!(page.body.contains("Held for someone else"));
}

#[tokio::test]
async fn the_row_form_shows_what_it_holds_now() {
    let mut world = world().await;
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains("Now: Education savings (529) · Someone else's, held for them"));
    assert!(details.contains("<option value=\"education\" selected>"));
    assert!(details.contains("value=\"Child A\""));
}

#[tokio::test]
async fn leaving_it_keeps_a_chosen_answer_over_a_change_of_kind() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("belongs_to", "household")],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "custodial"),
                ("belongs_to", ""),
                ("beneficiary", ""),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::Household);
    assert!(view.belongs_to_overridden);
    assert_counted(&view, true);
}

#[tokio::test]
async fn leaving_it_follows_the_kind_when_nothing_was_chosen() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "brokerage")]).await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "charitable"),
                ("belongs_to", ""),
                ("beneficiary", ""),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::GivenAway);
    assert!(!view.belongs_to_overridden);
    assert_counted(&view, false);
}

#[tokio::test]
async fn the_empty_answer_says_leave_it_when_it_follows_the_kind() {
    let mut world = world().await;
    bound(&mut world, "Plan", &[("kind", "brokerage")]).await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(&format!(
        "<option value=\"\" selected>{LEAVE_IT_FOLLOWING}</option>"
    )));
    assert!(!details.contains(FOLLOW_THE_KIND));
    assert!(details.contains(HOUSEHOLD_FOLLOWS));
}

#[tokio::test]
async fn the_empty_answer_says_leave_it_when_it_was_chosen() {
    let mut world = world().await;
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("belongs_to", "household")],
    )
    .await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(&format!(
        "<option value=\"\" selected>{LEAVE_IT_CHOSEN}</option>"
    )));
    assert!(!details.contains(FOLLOW_THE_KIND));
    assert!(!details.contains(LEAVE_IT_FOLLOWING));
    assert!(details.contains(STAYS));
}

#[tokio::test]
async fn a_row_held_for_someone_says_what_following_the_kind_would_do() {
    let mut world = world().await;
    bound(&mut world, "Plan", &[("kind", "education")]).await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(HELD_FOLLOWS));
}

#[tokio::test]
async fn a_held_row_says_first_that_moving_it_back_counts_it() {
    let mut world = world().await;
    bound(&mut world, "Plan", &[("kind", "education")]).await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(COUNTS_IT));
    assert!(!details.contains(TAKES_IT_OUT));
    assert!(details.contains("audit log"));
}

#[tokio::test]
async fn a_named_row_says_first_that_the_name_goes() {
    let mut world = world().await;
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(&format!("{HELD_FOLLOWS} and removes the name.")));
    assert!(details.contains(&format!("{COUNTS_IT}, and removes the name.")));
    assert!(details.contains("Empty it to remove the name."));
}

#[tokio::test]
async fn an_unnamed_row_says_nothing_of_a_name_going() {
    let mut world = world().await;
    bound(&mut world, "Plan", &[("kind", "education")]).await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(!details.contains("remove"));
}

#[tokio::test]
async fn a_household_row_says_first_that_moving_it_away_takes_it_out() {
    let mut world = world().await;
    bound(&mut world, "Everyday", &[("kind", "checking")]).await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(TAKES_IT_OUT));
    assert!(details.contains(COUNTS_IT_AGAIN));
    assert!(!details.contains(COUNTS_IT));
    assert!(details.contains("audit log"));
}

#[tokio::test]
async fn moving_it_back_turns_the_flags_on_and_audits_them() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", "household"),
                ("beneficiary", ""),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_counted(&world.view(fund).await, true);
    let audited = changes(&world.updates(fund).await[0]);
    let turned_on = json!({"before": false, "after": true});
    assert_eq!(audited["include_in_net_worth"], turned_on);
    assert_eq!(audited["include_in_forecast"], turned_on);
}

/// The box still holds the name the page filled in: that asks for nothing, so saying it's the
/// household's does what the form said it would rather than being refused.
#[tokio::test]
async fn moving_it_back_with_its_name_still_in_the_box_removes_the_name() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", "household"),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::Household);
    assert_eq!(view.beneficiary, None);
}

/// A kind that follows back to the household's, with the name still in the box.
#[tokio::test]
async fn following_the_kind_back_with_its_name_in_the_box_removes_the_name() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "brokerage"),
                ("belongs_to", ""),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::Household);
    assert_eq!(view.beneficiary, None);
    assert_counted(&view, true);
}

#[tokio::test]
async fn a_new_name_on_the_household_s_money_is_refused_without_echo() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", "household"),
                ("beneficiary", MARKED_NAME),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(!reply.body.contains(MARKED_NAME));
    assert_eq!(world.view(fund).await.belongs_to, BelongsTo::SomeoneElse);
}

#[tokio::test]
async fn the_row_form_changing_nothing_writes_nothing() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let reply = world
        .post_row(
            fund,
            &[
                ("kind", "education"),
                ("belongs_to", ""),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(world.updates(fund).await.len(), 0);
}

#[tokio::test]
async fn the_row_form_s_unknown_values_are_notices_without_echo() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    for fields in [
        [
            ("kind", "wish-kind"),
            ("belongs_to", ""),
            ("beneficiary", ""),
        ],
        [
            ("kind", "education"),
            ("belongs_to", "wish-kind"),
            ("beneficiary", ""),
        ],
    ] {
        let reply = world.post_row(fund, &fields).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(reply.body.contains("role=\"alert\""));
        assert!(!reply.body.contains("wish-kind"));
    }
    assert_eq!(world.updates(fund).await.len(), 0);
}

#[tokio::test]
async fn the_row_form_s_too_long_name_is_a_notice_without_echo() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let long = "Marked".repeat(11);
    let reply = world
        .post_row(fund, &[("kind", "education"), ("beneficiary", &long)])
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(!reply.body.contains("MarkedMarked"));
    assert_eq!(world.view(fund).await.beneficiary, None);
}

#[tokio::test]
async fn an_unknown_row_is_missing_whatever_the_form_says() {
    let mut world = world().await;
    for (path, kind) in [
        (row_path(AccountId::new()), "nonsense"),
        (
            "/connections/accounts/nonsense/belonging".to_owned(),
            "education",
        ),
    ] {
        let csrf = world.admin.get("/connections").await.csrf();
        let reply = world
            .admin
            .post_form(&path, &[("csrf", &csrf), ("kind", kind)])
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn the_row_form_is_only_for_who_may_write() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;

    let mut stranger = Browser::new(&world.server);
    stranger
        .post_form(
            &row_path(fund),
            &[("csrf", "AAAA"), ("belongs_to", "household")],
        )
        .await;
    assert_eq!(world.view(fund).await.belongs_to, BelongsTo::SomeoneElse);

    let mut bob = world.member().await;
    let page = bob.get("/connections").await;
    assert!(
        page.body
            .contains(&format!("action=\"{}\"", row_path(fund)))
    );
    let done = bob
        .post_form(
            &row_path(fund),
            &[
                ("csrf", &page.csrf()),
                ("kind", "education"),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(done.status, StatusCode::SEE_OTHER);
    assert_eq!(world.view(fund).await.beneficiary.as_deref(), Some(CHILD));
}
