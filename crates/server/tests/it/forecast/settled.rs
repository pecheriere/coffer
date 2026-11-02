//! A settled item in the forecast, through the real router (spec §8.9 R9a): its
//! occurrences in that month count as nothing, and the answer says so.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::answer::{Golden, golden};
use super::{
    FORECAST, ITEMS, WHAT_IF, World, goal_schedule, kinds, line, monthly, one_off_on, outflow, usd,
    with, world,
};

/// A firm household: 10,000.00 in Checking and a floor of 0.
pub(crate) async fn holding() -> (World, coffer_domain::ids::AccountId) {
    let world = world().await;
    let checking = world.funded("Checking", 1_000_000).await;
    world.floor(0).await;
    (world, checking)
}

impl World {
    /// Settles `item`, which must succeed.
    pub(crate) async fn settle(&self, item: &Value, month: &str, kind: &str) {
        let uri = settlement(item, month);
        self.admin_send(Method::PUT, &uri, &json!({"kind": kind}))
            .await;
    }
}

fn settlement(item: &Value, month: &str) -> String {
    format!("{ITEMS}/{}/settlements/{month}", item.as_str().unwrap())
}

/// The id of the line called `name` in `forecast`.
pub(crate) fn id_of(forecast: &Value, name: &str) -> Value {
    line(forecast, name)["source"]["id"].clone()
}

/// The only entry of `forecast`'s `settled`.
fn only_settled(forecast: &Value) -> &Value {
    let [only] = forecast["settled"].as_array().unwrap().as_slice() else {
        panic!("expected exactly one settled line");
    };
    only
}

fn no_line(forecast: &Value, name: &str) -> bool {
    !forecast["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line["name"] == name)
}

/// The worked example: as briefed the surplus is 3,836,000, provisional for November's
/// water bill. Settled, that bill's 12,000 gap goes: outflows 914,000 - 12,000 = 902,000,
/// needed 3,002,000, surplus 3,848,000, firm. The next water bill, January's, still counts.
#[tokio::test]
async fn settled_bill_drops_its_gap_and_its_caution() {
    let Golden { world, .. } = golden().await;
    let briefed = world.forecast().await;
    assert_eq!(briefed["surplus"], usd(3_836_000));
    assert_eq!(briefed["status"], "provisional");

    world
        .settle(&id_of(&briefed, "Water"), "2026-11", "paid")
        .await;
    let settled = world.forecast().await;

    assert_eq!(settled["surplus"], usd(3_848_000));
    assert_eq!(settled["status"], "firm");
    assert_eq!(settled["cautions"], json!([]));
    let totals = &settled["totals"][0];
    assert_eq!(totals["planned_outflows"], usd(902_000));
    assert_eq!(totals["cash_needed"], usd(3_002_000));
    let water = line(&settled, "Water");
    assert_eq!(water["unpaid"], usd(0));
    assert_eq!(water["gross"], usd(12_000));
    assert_eq!(water["first_on"], "2027-01-10");
    let projected: Vec<&Value> = settled["months"]
        .as_array()
        .unwrap()
        .iter()
        .map(|month| &month["lines"][0]["projected_cash"])
        .collect();
    assert_eq!(
        projected,
        [
            &usd(6_830_000),
            &usd(6_420_000),
            &usd(6_148_000),
            &usd(5_948_000)
        ]
    );
}

/// So no number silently differs: what was settled is on the answer, with its amount.
#[tokio::test]
async fn the_forecast_lists_what_was_settled() {
    let Golden {
        world, utilities, ..
    } = golden().await;
    let water = id_of(&world.forecast().await, "Water");

    world.settle(&water, "2026-11", "skipped").await;

    assert_eq!(
        *only_settled(&world.forecast().await),
        json!({
            "source": {"kind": "planned_item", "id": water},
            "name": "Water",
            "item_type": "recurring",
            "category": utilities,
            "month": "2026-11",
            "kind": "skipped",
            "occurrences": 1,
            "amount": usd(12_000),
        })
    );
}

/// `paid` and `skipped` say different things but count alike.
#[tokio::test]
async fn skipped_counts_like_paid() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");

    world.settle(&water, "2026-11", "skipped").await;

    let forecast = world.forecast().await;
    assert_eq!(forecast["surplus"], usd(3_848_000));
    assert_eq!(forecast["status"], "firm");
}

/// A one-off dated earlier this month with no spending owes 450.00 until it is settled.
#[tokio::test]
async fn settled_one_off_counts_as_nothing() {
    let (world, _) = holding().await;
    let health = world.expense("Health").await;
    let dentist = world
        .item(&outflow(
            "Dentist",
            45_000,
            health,
            &one_off_on("2026-11-10"),
        ))
        .await;
    assert_eq!(world.forecast().await["surplus"], usd(955_000));

    world.settle(&dentist, "2026-11", "paid").await;

    let forecast = world.forecast().await;
    assert_eq!(forecast["surplus"], usd(1_000_000));
    assert_eq!(forecast["status"], "firm");
    assert!(no_line(&forecast, "Dentist"), "nothing left of it counts");
    assert_eq!(only_settled(&forecast)["item_type"], "one_off");
}

/// 150.00 spent in Food, with a 100.00 bill on the 1st and a 300.00 budget on the 20th. The
/// bill takes 100.00 of it, so the budget still needs 300.00 - 50.00 = 250.00. Settled, the
/// bill takes none: the budget needs 300.00 - 150.00 = 150.00.
#[tokio::test]
async fn settled_bill_takes_no_share_of_the_pool() {
    let (world, checking) = holding().await;
    let food = world.expense("Food").await;
    let kit = world
        .item(&outflow("Kit", 10_000, food, &monthly("2026-01-01")))
        .await;
    world
        .item(&with(
            outflow("Groceries", 30_000, food, &monthly("2026-01-20")),
            "spending",
            json!("budget"),
        ))
        .await;
    world.spend(checking, "2026-11-05", 15_000, food).await;
    let query = "?horizon_months=1";
    let before = world.forecast_at(query).await;
    assert_eq!(line(&before, "Groceries")["counted"], usd(25_000));

    world.settle(&kit, "2026-11", "paid").await;

    let after = world.forecast_at(query).await;
    assert_eq!(line(&after, "Groceries")["reduced_by"], usd(15_000));
    assert_eq!(line(&after, "Groceries")["counted"], usd(15_000));
}

/// One 100.00 charge and two 100.00 one-offs: it pays the first and the second has a gap.
/// With the first settled, it pays the second.
#[tokio::test]
async fn spending_goes_to_the_next_bill_past_a_settled_one() {
    let (world, checking) = holding().await;
    let home = world.expense("Home").await;
    let first = world
        .item(&outflow("First", 10_000, home, &one_off_on("2026-11-02")))
        .await;
    world
        .item(&outflow("Second", 10_000, home, &one_off_on("2026-11-05")))
        .await;
    world.spend(checking, "2026-11-10", 10_000, home).await;
    let before = world.forecast().await;
    assert_eq!(line(&before, "Second")["unpaid"], usd(10_000));

    world.settle(&first, "2026-11", "paid").await;

    let after = world.forecast().await;
    assert!(no_line(&after, "Second"), "the charge paid it");
    assert_eq!(after["status"], "firm");
}

/// A 1,000.00 bill on the 20th: November, December and January, of which December is settled.
/// Settling it looks ahead, raises no caution, and leaves the other months alone.
#[tokio::test]
async fn a_future_settled_month_counts_as_nothing() {
    let (world, _) = holding().await;
    let housing = world.expense("Housing").await;
    let rent = world
        .item(&outflow("Rent", 100_000, housing, &monthly("2026-01-20")))
        .await;

    world.settle(&rent, "2026-12", "skipped").await;

    let forecast = world.forecast().await;
    assert_eq!(forecast["surplus"], usd(800_000));
    assert_eq!(forecast["status"], "firm");
    assert_eq!(forecast["cautions"], json!([]));
    let line = line(&forecast, "Rent");
    assert_eq!(line["occurrences"], 2);
    assert_eq!(line["gross"], usd(200_000));
    let outflows: Vec<&Value> = forecast["months"]
        .as_array()
        .unwrap()
        .iter()
        .map(|month| &month["lines"][0]["outflows"])
        .collect();
    assert_eq!(
        outflows,
        [&usd(100_000), &usd(0), &usd(100_000), &usd(0)],
        "November, December, January and part of February"
    );
    assert_eq!(only_settled(&forecast)["month"], "2026-12");
}

/// The answer before and after are the same whole answer.
#[tokio::test]
async fn deleting_a_settlement_restores_the_number() {
    let Golden { world, .. } = golden().await;
    let briefed = world.forecast().await;
    let water = id_of(&briefed, "Water");
    world.settle(&water, "2026-11", "paid").await;

    let (status, _) = world
        .send(
            Method::DELETE,
            &settlement(&water, "2026-11"),
            &world.admin,
            None,
        )
        .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(world.forecast().await == briefed, "as it was");
}

/// October is before the window's month and March after its end: neither changes anything
/// here, so neither is listed.
#[tokio::test]
async fn a_month_outside_the_window_changes_nothing() {
    let Golden { world, .. } = golden().await;
    let briefed = world.forecast().await;
    let rent = id_of(&briefed, "Rent");

    world.settle(&rent, "2026-10", "paid").await;
    world.settle(&rent, "2027-03", "paid").await;

    assert!(world.forecast().await == briefed, "as it was");
}

/// A what-if reads the plan as it stands, so a stored settlement is in it.
#[tokio::test]
async fn a_what_if_counts_what_is_settled() {
    let Golden { world, .. } = golden().await;
    let water = id_of(&world.forecast().await, "Water");

    world.settle(&water, "2026-11", "paid").await;

    let imagined = world.what_if(&json!({})).await;
    assert_eq!(imagined["surplus"], usd(3_848_000));
    assert_eq!(only_settled(&imagined)["month"], "2026-11");
}

/// Only the plan can be settled: a what-if has no way to say so, of itself or of an item.
#[tokio::test]
async fn a_what_if_cannot_settle() {
    let Golden { world, .. } = golden().await;
    let imagined = json!({
        "name": "Imagined",
        "amount": usd(100),
        "direction": "outflow",
        "schedule": one_off_on("2026-12-01"),
    });
    for body in [
        json!({"settled": ["2026-11"]}),
        json!({"settlements": [{"month": "2026-11", "kind": "paid"}]}),
        json!({"items": [with(imagined.clone(), "settled", json!(["2026-12"]))]}),
    ] {
        let (status, _) = world
            .send(Method::POST, WHAT_IF, &world.admin, Some(&body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

/// A months floor is how much the household wants held, not this month's bills: settling
/// November's occurrence leaves it, so settling can't lower the floor or zero it.
#[tokio::test]
async fn a_months_floor_keeps_a_settled_occurrence() {
    let world = world().await;
    world.funded("Checking", 1_000_000).await;
    world
        .settings(&json!({"floor": {"kind": "months_of_outflows", "months": 1}}))
        .await;
    let housing = world.expense("Housing").await;
    let rent = world
        .item(&outflow(
            "Rent",
            100_000,
            housing,
            &one_month_from_the_20th(),
        ))
        .await;

    world.settle(&rent, "2026-11", "paid").await;

    let forecast = world.forecast().await;
    assert_eq!(forecast["totals"][0]["floor"], usd(100_000));
    assert_eq!(forecast["status"], "firm");
}

/// A recurring bill whose only occurrence falls on 2026-11-20.
fn one_month_from_the_20th() -> Value {
    json!({
        "type": "recurring",
        "every": 1,
        "unit": "months",
        "anchor_on": "2026-11-20",
        "end_on": "2026-11-20",
    })
}

/// A one-off that counts as nothing can't be the goal's expense counted twice.
#[tokio::test]
async fn a_settled_one_off_raises_no_overlap_with_a_goal() {
    let (world, _) = holding().await;
    let travel = world.expense("Travel").await;
    world
        .item(&outflow(
            "Trip",
            100_000,
            travel,
            &goal_schedule("2026-06-01", "2027-01-31"),
        ))
        .await;
    let flights = world
        .item(&outflow(
            "Flights",
            50_000,
            travel,
            &one_off_on("2027-01-20"),
        ))
        .await;
    assert_eq!(
        kinds(&world.forecast().await, "cautions"),
        ["goal_one_off_overlap"]
    );

    world.settle(&flights, "2027-01", "skipped").await;

    assert_eq!(world.forecast().await["cautions"], json!([]));
}

/// Nothing is settled unless someone settled it: the list is there, and empty.
#[tokio::test]
async fn an_unsettled_forecast_lists_nothing() {
    let Golden { world, .. } = golden().await;
    let (status, text) = world.send(Method::GET, FORECAST, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let forecast: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(forecast["settled"], json!([]));
}
