//! Plan history through the forecast (money redesign §10.3, §10.7): a bill lowered from this
//! month stays paid by the payment it was paid with.

use axum::http::Method;
use serde_json::{Value, json};

use super::settled::holding;
use super::{line, monthly, outflow, usd};

const ONE_MONTH: &str = "?horizon_months=1";

/// Rent of 1,000.00 due on the 5th, paid 1,000.00 on 5 November, is lowered to 800.00 from this
/// month on the 15th: it is still paid, the payment is 200.00 more than the bill now asks, and
/// 800.00 is still due on 5 December.
#[tokio::test]
async fn lowering_a_paid_bill_keeps_it_paid() {
    let (world, checking) = holding().await;
    let rent = world.expense("Rent").await;
    let id = world
        .item(&outflow("Rent", 1_000, rent, &monthly("2026-11-05")))
        .await;
    let paid = world
        .spend_linked(checking, "2026-11-05", 1_000, rent, &Value::Null)
        .await;

    let (status, _) = world
        .send(
            Method::PATCH,
            &format!("/api/v1/plan-items/{}", id.as_str().unwrap()),
            &world.admin,
            Some(&json!({"amount": usd(800)})),
        )
        .await;
    assert!(status.is_success());
    let forecast = world.forecast_at(ONE_MONTH).await;

    let november = &line(&forecast, "Rent")["payments"][0];
    assert_eq!(november["on"], "2026-11-05");
    assert_eq!(november["paid_by"]["transaction"], paid);
    assert_eq!(november["paid_by"]["difference"], usd(200));
    assert_eq!(line(&forecast, "Rent")["unpaid"], usd(0));
    assert_eq!(line(&forecast, "Rent")["counted"], usd(800));
    assert_eq!(
        forecast["cautions"],
        json!([]),
        "nothing is unpaid or unclear"
    );
}
