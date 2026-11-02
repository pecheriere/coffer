//! A form older than the account: the row's form carries what it showed, and if that isn't the
//! account any more it changes nothing.

use axum::http::StatusCode;
use coffer_domain::accounts::BelongsTo;
use coffer_domain::ids::AccountId;

use super::super::{World, world};
use super::{CHILD, MARKED_NAME, assert_counted, bound};

const CHANGED: &str =
    "This account changed after the page opened, so this save wasn't applied. Look at it again.";

/// A form as the page drew it when the row held these.
fn shown<'a>(
    kind: &'a str,
    belongs_to: &'a str,
    chosen: &'a str,
    name: &'a str,
) -> [(&'a str, &'a str); 4] {
    [
        ("shown_kind", kind),
        ("shown_belongs_to", belongs_to),
        ("shown_chosen", chosen),
        ("shown_beneficiary", name),
    ]
}

/// Asserts that `sent` was refused as out of date, and changed nothing of `fund`.
async fn assert_refused_as_stale(world: &mut World, fund: AccountId, sent: &[(&str, &str)]) {
    let before = world.view(fund).await;
    let audited = world.updates(fund).await.len();
    let reply = world.post_exact(fund, sent).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body.contains("role=\"alert\""));
    assert!(reply.body.contains(CHANGED));
    let after = world.view(fund).await;
    assert_eq!(after.kind, before.kind);
    assert_eq!(after.belongs_to, before.belongs_to);
    assert_eq!(after.belongs_to_overridden, before.belongs_to_overridden);
    assert_eq!(after.beneficiary, before.beneficiary);
    assert_eq!(after.include_in_net_worth, before.include_in_net_worth);
    assert_eq!(after.include_in_forecast, before.include_in_forecast);
    assert_eq!(world.updates(fund).await.len(), audited);
}

#[tokio::test]
async fn an_untouched_stale_form_does_not_undo_another_move() {
    let mut world = world().await;
    let fund = bound(&mut world, "Shares", &[("kind", "brokerage")]).await;
    let moved = world
        .post_row(
            fund,
            &[
                ("kind", "custodial"),
                ("belongs_to", ""),
                ("beneficiary", CHILD),
            ],
        )
        .await;
    assert_eq!(moved.status, StatusCode::SEE_OTHER);
    let mut sent = shown("brokerage", "household", "false", "").to_vec();
    sent.extend([
        ("kind", "brokerage"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
    assert_counted(&world.view(fund).await, false);
}

#[tokio::test]
async fn a_stale_name_does_not_come_back() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", "Child B")],
    )
    .await;
    let mut sent = shown("education", "someone_else", "false", CHILD).to_vec();
    sent.extend([
        ("kind", "education"),
        ("belongs_to", ""),
        ("beneficiary", CHILD),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
    assert_eq!(
        world.view(fund).await.beneficiary.as_deref(),
        Some("Child B")
    );
}

#[tokio::test]
async fn a_stale_empty_box_does_not_remove_a_name_added_since() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", CHILD)],
    )
    .await;
    let mut sent = shown("education", "someone_else", "false", "").to_vec();
    sent.extend([
        ("kind", "education"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
    assert_eq!(world.view(fund).await.beneficiary.as_deref(), Some(CHILD));
}

#[tokio::test]
async fn a_stale_kind_alone_is_refused() {
    let mut world = world().await;
    let fund = bound(&mut world, "Shares", &[("kind", "retirement")]).await;
    let mut sent = shown("brokerage", "household", "false", "").to_vec();
    sent.extend([
        ("kind", "brokerage"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
}

#[tokio::test]
async fn a_stale_answer_alone_is_refused() {
    let mut world = world().await;
    let fund = bound(
        &mut world,
        "Shares",
        &[("kind", "brokerage"), ("belongs_to", "given_away")],
    )
    .await;
    let mut sent = shown("brokerage", "household", "true", "").to_vec();
    sent.extend([
        ("kind", "brokerage"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
}

#[tokio::test]
async fn a_form_saying_nothing_of_what_it_showed_is_refused_as_stale() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    assert_refused_as_stale(
        &mut world,
        fund,
        &[
            ("kind", "brokerage"),
            ("belongs_to", ""),
            ("beneficiary", ""),
        ],
    )
    .await;
}

#[tokio::test]
async fn a_stale_refusal_echoes_no_name() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "education")]).await;
    let mut sent = shown("education", "someone_else", "false", MARKED_NAME).to_vec();
    sent.extend([("beneficiary", "Marked Box Zq")]);
    let reply = world.post_exact(fund, &sent).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(!reply.body.contains(MARKED_NAME));
    assert!(!reply.body.contains("Marked Box Zq"));
}

/// What the page draws is what the check compares: the row's kind, answer and name, as sent back.
#[tokio::test]
async fn the_row_form_carries_what_it_showed() {
    let mut world = world().await;
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("beneficiary", "<b>Child</b> & co")],
    )
    .await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    for field in [
        r#"name="shown_kind" value="education""#,
        r#"name="shown_belongs_to" value="someone_else""#,
        r#"name="shown_chosen" value="false""#,
        r#"name="shown_beneficiary" value="&lt;b&gt;Child&lt;/b&gt; &amp; co""#,
    ] {
        assert!(details.contains(field), "{field}");
    }
}

/// A form whose words say the row follows its kind, when it no longer does, is out of date too:
/// its kind and answer are all still right, and a change of kind would not do what it said.
#[tokio::test]
async fn a_stale_choice_alone_is_refused() {
    let mut world = world().await;
    let fund = bound(&mut world, "Shares", &[("kind", "brokerage")]).await;
    let chose = world
        .post_row(
            fund,
            &[
                ("kind", "brokerage"),
                ("belongs_to", "household"),
                ("beneficiary", ""),
            ],
        )
        .await;
    assert_eq!(chose.status, StatusCode::SEE_OTHER);
    assert!(world.view(fund).await.belongs_to_overridden);
    let mut sent = shown("brokerage", "household", "false", "").to_vec();
    sent.extend([
        ("kind", "education"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    assert_refused_as_stale(&mut world, fund, &sent).await;
}

#[tokio::test]
async fn the_row_form_carries_that_its_answer_was_chosen() {
    let mut world = world().await;
    bound(
        &mut world,
        "Plan",
        &[("kind", "education"), ("belongs_to", "household")],
    )
    .await;
    let page = world.admin.get("/connections").await;
    let details = page.between("Kind and whose money it is", "</details>");
    assert!(details.contains(r#"name="shown_chosen" value="true""#));
}

/// The second press of a double submit finds the first one's result. The account did change, by
/// this same form, so the words mustn't say that nothing was saved.
#[tokio::test]
async fn a_second_submit_says_only_that_it_was_not_applied() {
    let mut world = world().await;
    let fund = bound(&mut world, "Plan", &[("kind", "brokerage")]).await;
    let mut sent = shown("brokerage", "household", "false", "").to_vec();
    sent.extend([
        ("kind", "custodial"),
        ("belongs_to", ""),
        ("beneficiary", ""),
    ]);
    let first = world.post_exact(fund, &sent).await;
    assert_eq!(first.status, StatusCode::SEE_OTHER);
    assert_refused_as_stale(&mut world, fund, &sent).await;
    assert_eq!(world.view(fund).await.belongs_to, BelongsTo::SomeoneElse);
}

/// What the box holds is compared after trimming, as the service stores a label, so a space typed
/// after the name still asks for nothing and the move removes the name.
#[tokio::test]
async fn moving_it_back_with_a_space_after_its_name_removes_the_name() {
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
                ("beneficiary", "Child A "),
            ],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let view = world.view(fund).await;
    assert_eq!(view.belongs_to, BelongsTo::Household);
    assert_eq!(view.beneficiary, None);
}
