//! Plan versus actual through the real router (spec §6.9, §8.9 R2, R6, R21): what the
//! store selects for a month, what it reports beside `actual`, and who may read it.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

mod access;
mod belonging;
mod flagged;
mod goals;
mod limits;
mod merged;
mod rollover;
mod rule_numbers;
mod settled;
mod transfer_links;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::policy::{AccountLevel, GrantTarget, TokenGrant};
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use jiff::Timestamp;
use secrecy::ExposeSecret as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const REPORT: &str = "/api/v1/reports/plan-vs-actual";
const ITEMS: &str = "/api/v1/plan-items";
const TRANSACTIONS: &str = "/api/v1/transactions";

const REMOVED: &str = "UPDATE transactions SET flag_source_removed = 1 WHERE id = ?";
const REVISED: &str = "UPDATE transactions SET flag_split_mismatch = 1 WHERE id = ?";
const DUPLICATE: &str = "UPDATE transactions SET flag_possible_duplicate = 1 WHERE id = ?";
const PENDING: &str = "UPDATE transactions SET pending = 1 WHERE id = ?";

/// The household's day in every world.
const TODAY: &str = "2026-11-15";

struct World {
    server: TestServer,
    alice: Actor,
    /// A token of alice's without grants, which may do anything.
    admin: SecretToken,
}

/// Admin "alice" on the day `TODAY` in UTC, with nothing else.
async fn world() -> World {
    world_on(TestServer::new().await).await
}

async fn world_on(server: TestServer) -> World {
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, admin) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("admin").unwrap(),
            Scope::Admin,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    server.clock.set(noon(TODAY));
    World {
        server,
        alice,
        admin,
    }
}

fn noon(day: &str) -> Timestamp {
    format!("{day}T12:00:00Z").parse().unwrap()
}

fn on(account: AccountId, level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::Account(account),
        level,
    }
}

fn all(level: AccountLevel) -> TokenGrant {
    TokenGrant {
        target: GrantTarget::All,
        level,
    }
}

fn usd(minor_units: i64) -> Value {
    json!({"minor_units": minor_units, "currency": "USD"})
}

/// A monthly recurring item of `minor_units` in `category`, anchored on `anchor_on`.
fn monthly(
    name: &str,
    direction: &str,
    minor_units: i64,
    category: CategoryId,
    anchor_on: &str,
) -> Value {
    json!({
        "name": name,
        "category": category,
        "amount": usd(minor_units),
        "direction": direction,
        "schedule": {"type": "recurring", "every": 1, "unit": "months", "anchor_on": anchor_on},
    })
}

/// A monthly budget of `minor_units` in `category`, on the 10th.
fn budget(name: &str, minor_units: i64, category: CategoryId) -> Value {
    let mut item = monthly(name, "outflow", minor_units, category, "2026-01-10");
    item["spending"] = json!("budget");
    item
}

/// A goal of `minor_units` in `category`, over the whole of 2026.
fn goal(name: &str, minor_units: i64, category: CategoryId) -> Value {
    json!({
        "name": name,
        "category": category,
        "amount": usd(minor_units),
        "direction": "outflow",
        "schedule": {"type": "goal", "start_on": "2026-01-01", "due_on": "2027-01-01"},
    })
}

/// The row of `report` for `category` in `month`, which must be the only one.
fn row<'a>(report: &'a Value, month: &str, category: CategoryId) -> &'a Value {
    let found: Vec<&Value> = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["month"] == month && row["category"] == json!(category))
        .collect();
    let [only] = found.as_slice() else {
        panic!("expected exactly one row");
    };
    only
}

fn is_empty(report: &Value, field: &str) -> bool {
    report[field].as_array().unwrap().is_empty()
}

impl World {
    /// A token of alice's with `scope` and `grants`.
    async fn token(&self, scope: Scope, grants: Vec<TokenGrant>) -> SecretToken {
        self.server
            .agent_tokens
            .create(
                &self.alice,
                TokenName::parse("claude").unwrap(),
                scope,
                None,
                grants,
            )
            .await
            .unwrap()
            .1
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &SecretToken,
        body: Option<&Value>,
    ) -> (StatusCode, String) {
        let request = Request::builder().method(method).uri(uri).header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.expose_secret()),
        );
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = router(self.server.app_state())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// Sends `body` as alice's admin token, which must succeed; returns the answer.
    async fn admin_send(&self, method: Method, uri: &str, body: &Value) -> Value {
        let (status, text) = self.send(method, uri, &self.admin, Some(body)).await;
        assert!(status.is_success(), "the setup must succeed: {status}");
        serde_json::from_str(&text).unwrap()
    }

    async fn holding(&self, name: &str, kind: &str) -> AccountId {
        self.holding_in(name, kind, "USD").await
    }

    async fn holding_in(&self, name: &str, kind: &str, currency: &str) -> AccountId {
        let answer = self
            .admin_send(
                Method::POST,
                "/api/v1/accounts",
                &json!({"name": name, "kind": kind, "currency": currency}),
            )
            .await;
        serde_json::from_value(answer["id"].clone()).unwrap()
    }

    async fn checking(&self) -> AccountId {
        self.holding("Checking", "checking").await
    }

    /// A category of `kind` in a group of its own.
    async fn category(&self, name: &str, kind: &str) -> CategoryId {
        let group = self
            .admin_send(
                Method::POST,
                "/api/v1/category-groups",
                &json!({"name": name}),
            )
            .await;
        let answer = self
            .admin_send(
                Method::POST,
                "/api/v1/categories",
                &json!({"group": group["id"], "name": name, "kind": kind}),
            )
            .await;
        serde_json::from_value(answer["id"].clone()).unwrap()
    }

    async fn expense(&self, name: &str) -> CategoryId {
        self.category(name, "expense").await
    }

    /// A stored planned item; returns its id.
    async fn item(&self, body: &Value) -> Value {
        self.admin_send(Method::POST, ITEMS, body).await["id"].clone()
    }

    /// An uncategorized manual transaction, negative for money out; returns its id.
    async fn manual(&self, account: AccountId, posted_on: &str, minor_units: i64) -> String {
        self.admin_send(
            Method::POST,
            TRANSACTIONS,
            &json!({
                "account": account,
                "posted_on": posted_on,
                "minor_units": minor_units,
                "description": "Purchase",
            }),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A manual transaction, negative for money out, with one split of all of it in
    /// `category`, linked to `goal` if not null; returns its id.
    async fn posted_linked(
        &self,
        account: AccountId,
        posted_on: &str,
        minor_units: i64,
        category: CategoryId,
        goal: &Value,
    ) -> String {
        let id = self.manual(account, posted_on, minor_units).await;
        self.admin_send(
            Method::PATCH,
            &format!("{TRANSACTIONS}/{id}"),
            &json!({"splits": [
                {"category": category, "minor_units": minor_units, "goal": goal}
            ]}),
        )
        .await;
        id
    }

    /// As [`Self::posted_linked`], linked to no goal.
    async fn posted(
        &self,
        account: AccountId,
        posted_on: &str,
        minor_units: i64,
        category: CategoryId,
    ) -> String {
        self.posted_linked(account, posted_on, minor_units, category, &Value::Null)
            .await
    }

    /// Plants a state on a transaction with `statement`. A sync's matching has its own
    /// tests, and only the state matters here.
    async fn plant(&self, statement: &'static str, transaction: &str) {
        self.server
            .store
            .execute_unchecked_for_tests(statement, transaction)
            .await
            .unwrap();
    }

    /// The report with this query string, which must answer 200.
    async fn report(&self, query: &str) -> Value {
        let (status, text) = self
            .send(Method::GET, &format!("{REPORT}?{query}"), &self.admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_str(&text).unwrap()
    }

    /// October 2026 alone, with an average of that month alone.
    async fn october(&self) -> Value {
        self.report("from=2026-10&months=1&average_months=1").await
    }
}

/// The hand check, over October with an average of two months:
///
/// - Food: 50,000 planned; 30,000 and 12,000 spent less 5,000 refunded is 37,000, leaving
///   a difference of 13,000. With September's 10,000 the average is 23,500.
/// - Housing: 200,000 planned and spent; alone in its window, the average is 100,000.
/// - Pay: 300,000 planned and received; the average is 150,000.
#[tokio::test]
async fn report_matches_seeded_month() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let housing = world.expense("Housing").await;
    let pay = world.category("Pay", "income").await;
    world.item(&budget("Groceries", 50_000, food)).await;
    world
        .item(&monthly("Rent", "outflow", 200_000, housing, "2026-01-01"))
        .await;
    world
        .item(&monthly("Salary", "inflow", 300_000, pay, "2026-01-25"))
        .await;
    world.posted(checking, "2026-09-12", -10_000, food).await;
    world.posted(checking, "2026-10-03", -30_000, food).await;
    world.posted(checking, "2026-10-20", -12_000, food).await;
    world.posted(checking, "2026-10-21", 5_000, food).await;
    world
        .posted(checking, "2026-10-01", -200_000, housing)
        .await;
    world.posted(checking, "2026-10-25", 300_000, pay).await;

    let report = world.report("from=2026-10&months=1&average_months=2").await;

    let expected = |category: CategoryId, figures: [i64; 4]| {
        json!({
            "month": "2026-10",
            "category": category,
            "currency": "USD",
            "planned": usd(figures[0]),
            "actual": usd(figures[1]),
            "difference": usd(figures[2]),
            "rolling_average": usd(figures[3]),
            "left_out": usd(0),
            "possible_duplicate": usd(0),
            "settled": usd(0),
        })
    };
    assert_eq!(
        report["rows"],
        json!([
            expected(food, [50_000, 37_000, 13_000, 23_500]),
            expected(housing, [200_000, 200_000, 0, 100_000]),
            expected(pay, [300_000, 300_000, 0, 150_000]),
        ])
    );
    assert_eq!(
        report["rollovers"],
        json!([{
            "category": food,
            "currency": "USD",
            "budgeted": usd(50_000),
            "spent": usd(37_000),
            "rollover": usd(13_000),
        }]),
        "the budget only: rent is a bill and pay an inflow"
    );
    assert!(is_empty(&report, "goals"), "no goal planned");
    assert!(
        is_empty(&report, "uncategorized"),
        "every split categorized"
    );
    assert!(is_empty(&report, "partial_months"), "October has passed");
}

#[tokio::test]
async fn months_and_average_default_to_three() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    world.posted(checking, "2026-07-20", -600, food).await;
    world.posted(checking, "2026-09-20", -300, food).await;
    world.posted(checking, "2026-12-20", -9, food).await;

    let report = world.report("from=2026-09").await;

    assert_eq!(
        row(&report, "2026-09", food)["rolling_average"],
        usd(300),
        "(600 + 0 + 300) / 3"
    );
    assert_eq!(
        row(&report, "2026-11", food)["rolling_average"],
        usd(100),
        "(300 + 0 + 0) / 3, a third month out"
    );
    assert!(
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["month"] != "2026-12"),
        "a fourth month isn't reported"
    );
}

#[tokio::test]
async fn the_months_first_and_last_days_are_in_it() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    world.posted(checking, "2026-09-30", -1, food).await;
    world.posted(checking, "2026-10-01", -10, food).await;
    world.posted(checking, "2026-10-31", -100, food).await;
    world.posted(checking, "2026-11-01", -1_000, food).await;

    let report = world.october().await;

    assert_eq!(row(&report, "2026-10", food)["actual"], usd(110));
    assert_eq!(report["rows"].as_array().unwrap().len(), 1, "October alone");
}

/// The average reaches before `from`, and only as far as its window.
#[tokio::test]
async fn the_rolling_average_reads_months_before_from() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    world.posted(checking, "2026-08-31", -9_000, food).await;
    world.posted(checking, "2026-09-01", -300, food).await;

    let report = world.report("from=2026-10&months=1&average_months=2").await;

    let row = row(&report, "2026-10", food);
    assert_eq!(row["actual"], usd(0));
    assert_eq!(row["rolling_average"], usd(150));
}

#[tokio::test]
async fn a_pending_transaction_is_not_actual() {
    let world = world().await;
    let checking = world.checking().await;
    let food = world.expense("Food").await;
    let pending = world.posted(checking, "2026-10-06", -500, food).await;
    world.plant(PENDING, &pending).await;

    assert!(is_empty(&world.october().await, "rows"), "not posted yet");
}

#[tokio::test]
async fn a_pending_transaction_is_not_uncategorized() {
    let world = world().await;
    let checking = world.checking().await;
    let pending = world.manual(checking, "2026-10-06", -500).await;
    world.plant(PENDING, &pending).await;

    assert!(
        is_empty(&world.october().await, "uncategorized"),
        "not posted yet"
    );
}

/// A pending charge linked to a goal isn't goal spending yet.
#[tokio::test]
async fn a_pending_transaction_is_not_goal_spending() {
    let world = world().await;
    let checking = world.checking().await;
    let home = world.expense("Home").await;
    let roof = world.item(&goal("Roof", 1_000, home)).await;
    let pending = world
        .posted_linked(checking, "2026-10-06", -500, home, &roof)
        .await;
    world.plant(PENDING, &pending).await;

    let report = world.october().await;

    assert_eq!(report["goals"][0]["spent_against"], usd(0));
    assert_eq!(report["goals"][0]["left_out"], usd(0));
}

#[tokio::test]
async fn a_transfer_category_is_not_actual() {
    let world = world().await;
    let checking = world.checking().await;
    let moved = world.category("Moved", "transfer").await;
    world.posted(checking, "2026-10-06", -500, moved).await;

    assert!(is_empty(&world.october().await, "rows"), "moved money");
}

/// Actuals count every account, whether or not the forecast does.
#[tokio::test]
async fn actuals_count_on_any_account() {
    let world = world().await;
    let brokerage = world.holding("Brokerage", "brokerage").await;
    let fees = world.expense("Fees").await;
    world.posted(brokerage, "2026-10-06", -500, fees).await;

    assert_eq!(
        row(&world.october().await, "2026-10", fees)["actual"],
        usd(500)
    );
}

#[tokio::test]
async fn an_inactive_item_plans_nothing() {
    let world = world().await;
    let food = world.expense("Food").await;
    let mut groceries = budget("Groceries", 50_000, food);
    groceries["active"] = json!(false);
    world.item(&groceries).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "nothing planned");
    assert!(is_empty(&report, "rollovers"), "nothing budgeted");
}

/// #43: a trade is uncategorized, and it isn't spending in any list here.
#[tokio::test]
async fn uncategorized_brokerage_trade_is_not_spending() {
    let world = world().await;
    let brokerage = world.holding("Brokerage", "brokerage").await;
    world.manual(brokerage, "2026-10-06", -500_000).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "no category");
    assert!(
        is_empty(&report, "uncategorized"),
        "and not spendable money"
    );
}

/// Money out, on cash and on a card, in its own list; money in isn't spending at all.
#[tokio::test]
async fn uncategorized_checking_spending_is_reported_separately() {
    let world = world().await;
    let checking = world.checking().await;
    let card = world.holding("Card", "credit_card").await;
    world.manual(checking, "2026-10-06", -500).await;
    world.manual(card, "2026-10-07", -70).await;
    world.manual(checking, "2026-10-08", 9_000).await;
    world.manual(checking, "2026-11-08", -3).await;

    let report = world.october().await;

    assert!(is_empty(&report, "rows"), "not spending");
    assert_eq!(
        report["uncategorized"],
        json!([{"month": "2026-10", "currency": "USD", "count": 2, "total": usd(570)}])
    );
}

/// A lookback month is read for averages, not reported.
#[tokio::test]
async fn uncategorized_months_are_the_reported_ones() {
    let world = world().await;
    let checking = world.checking().await;
    world.manual(checking, "2026-09-06", -500).await;
    world.manual(checking, "2026-10-06", -70).await;

    let report = world.report("from=2026-10&months=1&average_months=2").await;

    assert_eq!(
        report["uncategorized"],
        json!([{"month": "2026-10", "currency": "USD", "count": 1, "total": usd(70)}])
    );
}

/// One category spent from in two currencies: the SQL keeps them apart, and so does
/// every figure.
#[tokio::test]
async fn two_currencies_in_one_category_are_two_rows() {
    let world = world().await;
    let dollars = world.checking().await;
    let euros = world.holding_in("Euros", "checking", "EUR").await;
    let food = world.expense("Food").await;
    world.posted(dollars, "2026-10-06", -500, food).await;
    world.posted(euros, "2026-10-07", -70, food).await;

    let report = world.october().await;

    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row each");
    assert_eq!(rows[0]["currency"], "EUR");
    assert_eq!(
        rows[0]["actual"],
        json!({"minor_units": 70, "currency": "EUR"})
    );
    assert_eq!(rows[1]["currency"], "USD");
    assert_eq!(rows[1]["actual"], usd(500));
}

#[tokio::test]
async fn two_currencies_are_two_uncategorized_rows() {
    let world = world().await;
    let dollars = world.checking().await;
    let euros = world.holding_in("Euros", "checking", "EUR").await;
    world.manual(dollars, "2026-10-06", -500).await;
    world.manual(euros, "2026-10-07", -70).await;

    let report = world.october().await;

    assert_eq!(
        report["uncategorized"],
        json!([
            {
                "month": "2026-10",
                "currency": "EUR",
                "count": 1,
                "total": {"minor_units": 70, "currency": "EUR"},
            },
            {"month": "2026-10", "currency": "USD", "count": 1, "total": usd(500)},
        ])
    );
}

/// Which month holds today is the household's to say.
#[tokio::test]
async fn the_partial_month_follows_the_household_time_zone() {
    for (zone, partial) in [
        ("Pacific/Auckland", "2026-03"),
        ("America/Los_Angeles", "2026-02"),
    ] {
        let world = world_on(TestServer::with_env(&[("COFFER_TIMEZONE", zone)]).await).await;
        world
            .server
            .clock
            .set("2026-03-01T02:00:00Z".parse().unwrap());

        let report = world.report("from=2026-02&months=2").await;

        assert_eq!(report["partial_months"], json!([partial]));
    }
}

#[tokio::test]
async fn the_month_holding_today_is_partial() {
    let world = world().await;

    let report = world.report("from=2026-10&months=3").await;

    assert_eq!(report["partial_months"], json!(["2026-11"]));
}

/// Unspent across October and November; December hasn't happened, and rent is a bill.
#[tokio::test]
async fn rollover_covers_budgets_up_to_todays_month() {
    let world = world().await;
    let checking = world.checking().await;
    let house = world.expense("House").await;
    world.item(&budget("Repairs", 1_000, house)).await;
    world
        .item(&monthly("Rent", "outflow", 200_000, house, "2026-01-01"))
        .await;
    world.posted(checking, "2026-10-01", -200_000, house).await;
    world.posted(checking, "2026-10-12", -300, house).await;
    world.posted(checking, "2026-11-01", -200_000, house).await;
    world.posted(checking, "2026-11-12", -1_400, house).await;

    let report = world.report("from=2026-10&months=3&average_months=1").await;

    assert_eq!(
        report["rollovers"],
        json!([{
            "category": house,
            "currency": "USD",
            "budgeted": usd(2_000),
            "spent": usd(1_700),
            "rollover": usd(300),
        }])
    );
}

#[tokio::test]
async fn a_bill_has_no_rollover() {
    let world = world().await;
    let checking = world.checking().await;
    let housing = world.expense("Housing").await;
    world
        .item(&monthly("Rent", "outflow", 200_000, housing, "2026-01-01"))
        .await;
    world
        .posted(checking, "2026-10-01", -100_000, housing)
        .await;

    assert!(is_empty(&world.october().await, "rollovers"), "a bill");
}

#[tokio::test]
async fn months_out_of_range_is_422() {
    let world = world().await;
    for query in ["from=2026-10&months=0", "from=2026-10&months=37"] {
        let uri = format!("{REPORT}?{query}");
        let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let uri = format!("{REPORT}?from=2026-10&months=36");
    let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK, "36 months is the limit");
}

#[tokio::test]
async fn average_months_out_of_range_is_422() {
    let world = world().await;
    for query in [
        "from=2026-10&average_months=0",
        "from=2026-10&average_months=13",
    ] {
        let uri = format!("{REPORT}?{query}");
        let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let uri = format!("{REPORT}?from=2026-10&average_months=12");
    let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::OK, "12 months is the limit");
}

/// A month that parses, but that no plan can reach: 9999-12's successor is itself.
#[tokio::test]
async fn from_outside_the_plan_years_is_422() {
    let world = world().await;
    for from in ["1969-12", "2201-01", "9999-12", "0000-01"] {
        let uri = format!("{REPORT}?from={from}");
        let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!text.contains(from), "the refusal echoes nothing");
    }
}

#[tokio::test]
async fn from_at_the_ends_of_the_plan_years_answers() {
    let world = world().await;
    for query in [
        "from=1970-01&months=36&average_months=12",
        "from=2200-12&months=36&average_months=12",
    ] {
        let uri = format!("{REPORT}?{query}");
        let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// Too big for the query's integer type is refused by its parser, as on the forecast.
#[tokio::test]
async fn months_past_the_query_type_is_400() {
    let world = world().await;
    let uri = format!("{REPORT}?from=2026-10&months=300");
    let (status, _) = world.send(Method::GET, &uri, &world.admin, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bad_month_is_400() {
    let world = world().await;
    for from in ["2026-13", "2026-1", "2026-10-01", "soon", ""] {
        let uri = format!("{REPORT}?from={from}");
        let (status, text) = world.send(Method::GET, &uri, &world.admin, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            from.is_empty() || !text.contains(from),
            "the refusal echoes nothing"
        );
    }
    let (status, _) = world.send(Method::GET, REPORT, &world.admin, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "from is required");
}
