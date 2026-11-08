//! Notes over more than one line, as a browser sends them: a textarea's line breaks come as
//! `\r\n`, and the HTML parser drops one line break right after `<textarea>`.

use axum::http::{Method, StatusCode};
use serde_json::json;

use super::{PAGE, admin, item, monthly_bill, post, with, with_all};
use crate::forecast::{World, world};

/// What a browser sends for the notes textarea of the form at `body`: its text with the one
/// line break the HTML parser drops after the opening tag left out, and every line break as
/// `\r\n`.
fn sent_notes(body: &str) -> String {
    let (_, after) = body.split_once("name=\"notes\"").unwrap();
    let (_, inside) = after.split_once('>').unwrap();
    let (text, _) = inside.split_once("</textarea>").unwrap();
    let value = text.strip_prefix('\n').unwrap_or(text);
    value.replace('\n', "\r\n")
}

/// The id of the API's item named `name`.
async fn id_of(world: &World, name: &str) -> String {
    item(world, name).await["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn notes_typed_over_lines_are_kept_with_plain_line_breaks() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let bill = monthly_bill("Rent", &rent);

    let reply = post(
        &mut admin,
        PAGE,
        &with(bill.clone(), "notes", "Line one\r\nLine two"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(item(&world, "Rent").await["notes"], "Line one\nLine two");

    let path = format!("{PAGE}/{}", id_of(&world, "Rent").await);
    let changed = with(bill, "notes", "Line one\r\nLine two\rLine three");
    let reply = post(&mut admin, &path, &changed).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(
        item(&world, "Rent").await["notes"],
        "Line one\nLine two\nLine three",
        "a lone \\r is a line break too"
    );
}

#[tokio::test]
async fn notes_written_through_the_api_survive_a_save_from_the_page() {
    let world = world().await;
    let mut admin = admin(&world).await;
    let rent = world.expense("Rent").await.to_string();
    let bill = monthly_bill("Rent", &rent);
    post(&mut admin, PAGE, &bill).await;
    let path = format!("{PAGE}/{}", id_of(&world, "Rent").await);
    let notes = "\nStarts with a line break\nand goes on";
    let uri = format!("/api/v1/plan-items/{}", id_of(&world, "Rent").await);
    let (status, _) = world
        .send(
            Method::PATCH,
            &uri,
            &world.admin,
            Some(&json!({"notes": notes})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let form = admin.get(&path).await.body;
    let sent = sent_notes(&form);
    assert_eq!(sent, "\r\nStarts with a line break\r\nand goes on");
    let reply = post(
        &mut admin,
        &path,
        &with_all(bill, &[("amount", "1300"), ("notes", &sent)]),
    )
    .await;

    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let saved = item(&world, "Rent").await;
    assert_eq!(saved["amount"]["minor_units"], 130_000);
    assert_eq!(saved["notes"], notes, "the notes are as they were");
}
