//! Plan history through the forecast (money redesign §10.3, §10.7): a bill lowered from this
//! month stays paid by the payment it was paid with.

use axum::http::Method;
use serde_json::{Value, json};

use super::settled::holding;
use super::{World, line, monthly, one_off_on, outflow, usd};

const ONE_MONTH: &str = "?horizon_months=1";

impl World {
    /// Free money as the admin reads it, which must answer 200.
    async fn free_money_body(&self) -> Value {
        let (status, text) = self
            .send(Method::GET, "/api/v1/free-money", &self.admin, None)
            .await;
        assert!(status.is_success());
        serde_json::from_str(&text).unwrap()
    }
}

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
    let money = world.free_money_body().await;
    assert_eq!(money["cautions"], json!([]), "{money}");
    let need = &money["currencies"][0]["still_need"];
    assert_eq!(need["must_pay_this_month"], usd(0), "November's is paid");
    assert_eq!(need["must_pay_coming"], usd(800), "{money}");
}

/// A one-off of 1,000.00 due on 20 November, paid on the 14th, is lowered to 800.00 from this
/// month: it is matched like a bill, so it stays paid, in the forecast and in free money.
#[tokio::test]
async fn lowering_a_paid_one_off_keeps_it_paid() {
    let (world, checking) = holding().await;
    let tyres = world.expense("Tyres").await;
    let id = world
        .item(&outflow("Tyres", 1_000, tyres, &one_off_on("2026-11-20")))
        .await;
    let paid = world
        .spend_linked(checking, "2026-11-14", 1_000, tyres, &Value::Null)
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

    let tyres = line(&forecast, "Tyres");
    assert_eq!(tyres["payments"][0]["paid_by"]["transaction"], paid);
    assert_eq!(tyres["payments"][0]["paid_by"]["difference"], usd(200));
    assert_eq!(tyres["counted"], usd(0));
    assert_eq!(forecast["cautions"], json!([]));
    let money = world.free_money_body().await;
    assert_eq!(money["cautions"], json!([]), "{money}");
    assert_eq!(
        money["currencies"][0]["still_need"]["total"],
        usd(0),
        "{money}"
    );
}
