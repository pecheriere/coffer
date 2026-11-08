//! A transaction's transfer link: the legs coffer suggests, linking one or any other, what the
//! page says of a link, and removing it.

use axum::http::StatusCode;
use coffer_domain::ids::{AccountId, TransactionId};
use jiff::civil::date;
use serde_json::json;

use super::{World, account, assert_redirect, assert_refused, detail, member, signed_in, world};

/// Alice's "Checking" and "Savings", kept by hand, with an outflow of `out` from checking on
/// 2 October and an inflow of `into` to savings on the 3rd.
async fn legs(world: &mut World, out: i64, into: i64) -> (TransactionId, TransactionId, AccountId) {
    let checking = account(&world.server, &world.alice, "Checking", "USD").await;
    let savings = account(&world.server, &world.alice, "Savings", "USD").await;
    let outflow = world
        .manual_in(checking, out, "To savings", date(2026, 10, 2))
        .await;
    let inflow = world
        .manual_in(savings, into, "From checking", date(2026, 10, 3))
        .await;
    (outflow, inflow, savings)
}

#[tokio::test]
async fn a_suggested_leg_is_linked_and_the_link_removed() {
    let mut world = world().await;
    let (outflow, inflow, _) = legs(&mut world, -50_000, 50_000).await;
    let page = world.page(&detail(inflow)).await;
    assert!(page.body.contains("These might be its other leg"));
    assert!(page.body.contains("aria-label=\"Link with To savings\""));

    let reply = world
        .post(
            &format!("{}/link", detail(inflow)),
            &[("other", &outflow.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(inflow));
    let read = world.read(inflow).await;
    let link = read["transfer_link"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        read["transfer_link"]["other_transaction"],
        outflow.to_string()
    );
    let stored = world
        .api(&format!("/api/v1/transfer-links/{link}"), StatusCode::OK)
        .await;
    assert_eq!(stored["outflow"]["transaction"], outflow.to_string());
    assert_eq!(stored["inflow"]["transaction"], inflow.to_string());
    let event = &world.audit(&link).await[0];
    assert_eq!(event.action, "transfer_link.create");
    assert_eq!(event.actor, signed_in(&world.alice));

    let page = world.page(&detail(outflow)).await;
    assert!(
        page.body
            .contains("500.00 USD left Checking on 2 October 2026")
    );
    assert!(
        page.body
            .contains("500.00 USD reached Savings on 3 October 2026")
    );
    assert!(
        page.body
            .contains(&format!("href=\"/transactions/{inflow}\">The other leg"))
    );
    assert!(
        page.body.contains("is one movement"),
        "both accounts are counted cash"
    );

    let reply = world
        .post(&format!("{}/unlink", detail(outflow)), &[])
        .await;
    assert_redirect(&reply, &detail(outflow));
    assert_eq!(world.read(outflow).await["transfer_link"], json!(null));
    world
        .api(
            &format!("/api/v1/transfer-links/{link}"),
            StatusCode::NOT_FOUND,
        )
        .await;
    assert_eq!(world.audit(&link).await[0].action, "transfer_link.remove");
}

#[tokio::test]
async fn any_other_leg_is_linked_by_its_address_and_the_difference_said() {
    let mut world = world().await;
    let (outflow, inflow, _) = legs(&mut world, -50_000, 49_500).await;
    let page = world.page(&detail(outflow)).await;
    assert!(
        page.body
            .contains("No transaction you can see looks like its other leg.")
    );

    let address = format!("https://coffer.example/transactions/{inflow}");
    let reply = world
        .post(&format!("{}/link", detail(outflow)), &[("other", &address)])
        .await;
    assert_redirect(&reply, &detail(outflow));
    let page = world.page(&detail(outflow)).await;
    assert!(
        page.body
            .contains("5.00 USD less arrived than left, such as for a fee.")
    );
}

#[tokio::test]
async fn legs_in_two_currencies_say_how_the_rate_check_went() {
    let mut world = world().await;
    let checking = account(&world.server, &world.alice, "Checking", "USD").await;
    let euros = account(&world.server, &world.alice, "Euros", "EUR").await;
    let outflow = world
        .manual_in(checking, -11_000, "To euros", date(2026, 10, 2))
        .await;
    let inflow = world
        .manual_in(euros, 10_000, "From dollars", date(2026, 10, 2))
        .await;

    let reply = world
        .post(
            &format!("{}/link", detail(inflow)),
            &[("other", &outflow.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(inflow));
    let page = world.page(&detail(inflow)).await;
    assert!(page.body.contains("110.00 USD left Checking"));
    assert!(page.body.contains("100.00 EUR reached Euros"));
    assert!(
        page.body
            .contains("No usable exchange rate is held for that day")
    );
}

#[tokio::test]
async fn a_link_the_service_refuses_is_said_in_its_words() {
    let mut world = world().await;
    let (outflow, _, savings) = legs(&mut world, -50_000, 50_000).await;
    let also_out = world
        .manual_in(savings, -2_000, "Cash out", date(2026, 10, 4))
        .await;
    let path = format!("{}/link", detail(outflow));

    let reply = world.post(&path, &[("other", &also_out.to_string())]).await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Link money out with money in: these two go the same way.",
        "xyzzy",
    );
    assert!(!reply.body.contains("`inflow`"), "no API field names");
    let reply = world.post(&path, &[("other", "xyzzy")]).await;
    assert_refused(
        &reply,
        StatusCode::UNPROCESSABLE_ENTITY,
        "not the address of a transaction",
        "xyzzy",
    );
    let reply = world
        .post(&path, &[("other", &TransactionId::new().to_string())])
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = world
        .post(&format!("{}/unlink", detail(outflow)), &[])
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "no link to remove");
    assert_eq!(world.read(outflow).await["transfer_link"], json!(null));
}

/// The unlink form names no link: the one removed is the path's transaction's, whatever is
/// posted, so a crafted form can't end another transaction's link.
#[tokio::test]
async fn unlinking_removes_only_the_paths_own_link() {
    let mut world = world().await;
    let (outflow, inflow, savings) = legs(&mut world, -50_000, 50_000).await;
    let brokerage = account(&world.server, &world.alice, "Brokerage", "USD").await;
    let other_out = world
        .manual_in(savings, -7_000, "To brokerage", date(2026, 10, 5))
        .await;
    let other_in = world
        .manual_in(brokerage, 7_000, "From savings", date(2026, 10, 5))
        .await;
    for (from, to) in [(outflow, inflow), (other_in, other_out)] {
        let reply = world
            .post(
                &format!("{}/link", detail(from)),
                &[("other", &to.to_string())],
            )
            .await;
        assert_redirect(&reply, &detail(from));
    }
    let other_link = world.read(other_out).await["transfer_link"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let reply = world
        .post(
            &format!("{}/unlink", detail(outflow)),
            &[("link", &other_link)],
        )
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no such field"
    );
    let reply = world
        .post(&format!("{}/unlink", detail(outflow)), &[])
        .await;
    assert_redirect(&reply, &detail(outflow));

    assert_eq!(world.read(outflow).await["transfer_link"], json!(null));
    assert_eq!(
        world.read(other_out).await["transfer_link"]["id"],
        other_link
    );
    let reply = world
        .post(&format!("{}/unlink", detail(TransactionId::new())), &[])
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        world.read(other_in).await["transfer_link"]["id"],
        other_link
    );
}

/// A member links on this page as the API lets a member do: on every account, as themselves.
#[tokio::test]
async fn a_member_links_as_themselves() {
    let mut world = world().await;
    let (outflow, inflow, _) = legs(&mut world, -50_000, 50_000).await;
    let (mut bob, bob_actor) = member(&world).await;
    let page = bob.get(&detail(outflow)).await;
    assert_eq!(page.status, StatusCode::OK);
    let reply = bob
        .post_form(
            &format!("{}/link", detail(outflow)),
            &[("csrf", &page.csrf()), ("other", &inflow.to_string())],
        )
        .await;
    assert_redirect(&reply, &detail(outflow));
    let link = world.read(outflow).await["transfer_link"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(world.audit(&link).await[0].actor, signed_in(&bob_actor));
}

#[tokio::test]
async fn the_link_forms_need_the_sessions_token_and_refuse_unknown_fields() {
    let mut world = world().await;
    let (outflow, inflow, _) = legs(&mut world, -50_000, 50_000).await;
    let inflow = inflow.to_string();
    let link: &[(&str, &str)] = &[("other", &inflow)];
    let link_path = format!("{}/link", detail(outflow));
    let unlink_path = format!("{}/unlink", detail(outflow));
    refuses_forged_and_extra(&mut world, &link_path, link).await;
    assert_eq!(world.read(outflow).await["transfer_link"], json!(null));

    assert_redirect(&world.post(&link_path, link).await, &detail(outflow));
    refuses_forged_and_extra(&mut world, &unlink_path, &[]).await;
    assert_eq!(
        world.read(outflow).await["transfer_link"]["other_transaction"],
        inflow
    );
}

/// `fields` posted to `path` with a forged token are refused, and with the session's token and
/// a field the form doesn't have.
async fn refuses_forged_and_extra(world: &mut World, path: &str, fields: &[(&str, &str)]) {
    let mut forged = vec![("csrf", "forged")];
    forged.extend_from_slice(fields);
    let reply = world.browser.post_form(path, &forged).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let mut extra = fields.to_vec();
    extra.push(("admin", "yes"));
    let reply = world.post(path, &extra).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
}
