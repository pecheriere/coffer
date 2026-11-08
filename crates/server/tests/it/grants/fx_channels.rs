//! Past the report itself: rates shared between accounts, the FX endpoints, the display
//! currency setting, what a write token can try, and the audit log.

use axum::http::{Method, StatusCode};
use coffer_domain::fx::FxFetchStatus;
use coffer_domain::identity::Scope;
use coffer_domain::ids::AccountId;
use coffer_domain::policy::AccountLevel::{Balances, Transactions, Write};
use coffer_domain::time::Clock as _;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use coffer_store::KeptBalance;
use serde_json::{Value, json};

use super::net_worth_channels::{
    Answer, HIDDEN_MINOR, HIDDEN_NAME, RANGE, Scene, TODAY, VISIBLE_MINOR, all, assert_identical,
    assert_same_for_the_token, code, day, money, scene, scene_over,
};
use super::{World, on, world_over};

const SETTINGS: &str = "/api/v1/household/settings";
const COVERAGE: &str = "/api/v1/fx/coverage";
const RATES_THIS_YEAR: &str = "from=2026-01-01&to=2026-12-31";

/// What a token refused the FX endpoints or a change of display currency gets.
const NEEDS_ALL_AT_BALANCES: &str = "this action needs balances access to every account";

fn rates_of(currency: &str) -> String {
    format!("/api/v1/fx/rates?currency={currency}&{RATES_THIS_YEAR}")
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// The last point of `report`.
fn latest(report: &Value) -> &Value {
    report["points"].as_array().unwrap().last().unwrap()
}

/// A hidden JPY account with a large balance, and everything it makes the household fetch.
async fn hide_yen(world: &World) -> AccountId {
    let hidden = world.holding(HIDDEN_NAME, "JPY").await;
    world.balance_on(hidden, "2026-10-26", HIDDEN_MINOR).await;
    world.rates("JPY", &[("2026-10-01", "175.68")]).await;
    world.rates("USD", &[("2026-10-01", "1.1188")]).await;
    hidden
}

/// Every token that doesn't see every account, at every scope, for `scene`.
async fn restricted_tokens(scene: &Scene) -> Vec<SecretToken> {
    let mut tokens = Vec::new();
    for (scope, level) in [
        (Scope::Read, Balances),
        (Scope::Read, Transactions),
        (Scope::Write, Write),
        (Scope::Admin, Write),
    ] {
        let (_, token) = scene
            .world
            .token(scope, vec![on(scene.visible, level)])
            .await;
        tokens.push(token);
    }
    tokens
}

/// `uri` as every token of `tokens` gets it.
async fn for_each(scene: &Scene, tokens: &[SecretToken], uri: &str) -> Vec<Answer> {
    let mut answers = Vec::new();
    for token in tokens {
        answers.push(scene.whole(uri, token).await);
    }
    answers
}

// Shared rates. A rate is reference data, stored once for the household, so a visible
// currency that a hidden account already caused to be fetched has rates sooner than it would
// have alone. The ruling, in the spec's N2: a held rate reads the same whoever caused it (the
// first test below), and what is left is one bit about a currency the token already sees, the
// same bit whether the wait is hours or an outage. The next tests pin that it is no more.

/// A visible pound account with display USD, and the pair of rates its row needs.
async fn visible_pounds(scene: &Scene) -> (AccountId, SecretToken) {
    let pounds = scene.world.holding("Pounds", "GBP").await;
    scene
        .world
        .balance_on(pounds, "2026-10-26", VISIBLE_MINOR)
        .await;
    let (_, token) = scene
        .world
        .token(Scope::Read, vec![on(pounds, Balances)])
        .await;
    (pounds, token)
}

async fn fetched_pounds_and_dollars(world: &World) {
    world.rates("GBP", &[("2026-09-01", "0.85")]).await;
    world.rates("USD", &[("2026-09-01", "1.1")]).await;
}

/// `token`'s report with the account's id named.
async fn report_for(scene: &Scene, pounds: AccountId, token: &SecretToken) -> String {
    scene
        .whole(RANGE, token)
        .await
        .3
        .replace(&pounds.to_string(), "<pounds>")
}

/// Whoever caused the fetch, the same rates give the same bytes, so what a report shows of a
/// hidden account's use of a currency is nothing but those rates being there.
#[tokio::test]
async fn a_held_rate_reads_the_same_whoever_caused_it() {
    let (alone, beside) = (scene().await, scene().await);
    let (own, own_token) = visible_pounds(&alone).await;
    let (shared, shared_token) = visible_pounds(&beside).await;
    let hidden = beside.world.holding(HIDDEN_NAME, "GBP").await;
    beside
        .world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;
    for scene in [&alone, &beside] {
        fetched_pounds_and_dollars(&scene.world).await;
    }

    assert!(
        report_for(&alone, own, &own_token).await
            == report_for(&beside, shared, &shared_token).await,
        "byte-identical"
    );
}

/// Before the job first runs for a new account, the token can tell whether its currency was
/// already held: the accepted difference, `not_fetched_yet` against an amount, and no more.
#[tokio::test]
async fn a_rate_a_hidden_use_fetched_early_shows_only_as_an_amount() {
    let (alone, beside) = (scene().await, scene().await);
    let (_, own_token) = visible_pounds(&alone).await;
    let (_, shared_token) = visible_pounds(&beside).await;
    let hidden = beside.world.holding(HIDDEN_NAME, "GBP").await;
    beside
        .world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;
    fetched_pounds_and_dollars(&beside.world).await;

    let waiting = alone.report(&own_token).await;
    let converted = beside.report(&shared_token).await;

    assert_eq!(latest(&waiting)["total"], Value::Null);
    assert_eq!(waiting["status"], "provisional");
    let reasons: Vec<_> = waiting["cautions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|caution| (&caution["kind"], &caution["reason"]))
        .collect();
    let not_fetched = (&json!("rate_missing"), &json!("not_fetched_yet"));
    assert!(reasons.iter().all(|reason| *reason == not_fetched));
    assert_eq!(latest(&converted)["total"], money(129_412, "USD"));
    assert_eq!(converted["accounts"].as_array().unwrap().len(), 1);
}

/// The long case: rates a hidden use fetched, then a fetch failing for as long as it lasts.
/// The token's amount is at the last rate held, with its date, and `rate_stale`, where alone
/// it would have `fetch_failing` and no total. It learns one bit about a currency it holds, and
/// a date: some use of it predates the outage, and the last fetch was about `rate_on`. That
/// use could be an account of its own or a past display currency, and it says nothing of any
/// amount, how many, or whether one still exists. It reads the same as a household that
/// fetched for itself before the outage.
#[tokio::test]
async fn an_outage_after_hidden_use_shows_only_a_held_rate_and_its_date() {
    let (never, alone, beside) = (scene().await, scene().await, scene().await);
    let (_, never_token) = visible_pounds(&never).await;
    let (own, own_token) = visible_pounds(&alone).await;
    let (shared, shared_token) = visible_pounds(&beside).await;
    let hidden = beside.world.holding(HIDDEN_NAME, "GBP").await;
    beside
        .world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;
    for currency in ["GBP", "USD"] {
        never
            .world
            .fetch_failed(currency, FxFetchStatus::Failing)
            .await;
    }
    for scene in [&alone, &beside] {
        fetched_pounds_and_dollars(&scene.world).await;
        for currency in ["GBP", "USD"] {
            scene
                .world
                .fetch_failed(currency, FxFetchStatus::Failing)
                .await;
        }
    }

    let failing = never.report(&never_token).await;
    assert_eq!(latest(&failing)["total"], Value::Null);
    assert_eq!(failing["cautions"][0]["reason"], "fetch_failing");

    let held = beside.report(&shared_token).await;
    assert_eq!(latest(&held)["total"], money(129_412, "USD"));
    let stale: Vec<_> = held["cautions"].as_array().unwrap().iter().collect();
    assert!(stale.iter().all(|caution| caution["kind"] == "rate_stale"));
    assert!(
        stale
            .iter()
            .all(|caution| caution["rate_on"] == "2026-09-01")
    );
    assert!(
        report_for(&alone, own, &own_token).await
            == report_for(&beside, shared, &shared_token).await,
        "byte-identical to fetching for itself"
    );
}

/// The same with the provider turned off after hidden use: `provider_off` for a household
/// that never held the rates, the old rates for one that did, however they came.
#[tokio::test]
async fn a_provider_off_after_hidden_use_shows_only_a_held_rate_and_its_date() {
    async fn off() -> Scene {
        let mut server = TestServer::with_env(&[("COFFER_FX_PROVIDER", "none")]).await;
        let fake = server.use_fake_provider();
        let scene = scene_over(world_over(server, fake).await).await;
        scene.world.rebuild(TODAY).await;
        scene
    }
    let (never, alone, beside) = (off().await, off().await, off().await);
    let (_, never_token) = visible_pounds(&never).await;
    let (own, own_token) = visible_pounds(&alone).await;
    let (shared, shared_token) = visible_pounds(&beside).await;
    let hidden = beside.world.holding(HIDDEN_NAME, "GBP").await;
    beside
        .world
        .balance_on(hidden, "2026-10-26", HIDDEN_MINOR)
        .await;
    for scene in [&alone, &beside] {
        fetched_pounds_and_dollars(&scene.world).await;
    }

    assert_eq!(
        never.report(&never_token).await["cautions"][0]["reason"],
        "provider_off"
    );
    assert_eq!(
        latest(&beside.report(&shared_token).await)["total"],
        money(129_412, "USD")
    );
    assert!(
        report_for(&alone, own, &own_token).await
            == report_for(&beside, shared, &shared_token).await,
        "byte-identical"
    );
}

// The FX endpoints.

/// Below `all` at `balances` they refuse with one body, whatever the currency, whether or not
/// it is in use, and before looking at the range.
#[tokio::test]
async fn fx_endpoints_refuse_alike() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    held.world.fetch_failed("THB", FxFetchStatus::Failing).await;
    let uris = [
        COVERAGE.to_owned(),
        rates_of("JPY"),
        rates_of("THB"),
        rates_of("USD"),
        rates_of("EUR"),
        rates_of("CHF"),
        "/api/v1/fx/rates?currency=JPY&from=2026-12-31&to=2026-01-01".to_owned(),
        "/api/v1/fx/rates?currency=CHF&from=2026-12-31&to=2026-01-01".to_owned(),
        "/api/v1/fx/rates?currency=JPY&from=2000-01-01&to=2026-12-31".to_owned(),
    ];
    let (bare_tokens, held_tokens) = (
        restricted_tokens(&bare).await,
        restricted_tokens(&held).await,
    );
    let mut bodies = Vec::new();
    for uri in &uris {
        let answers = for_each(&held, &held_tokens, uri).await;
        assert_identical(&for_each(&bare, &bare_tokens, uri).await, &answers);
        for (_, status, _, body) in answers {
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(parsed(&body)["detail"], NEEDS_ALL_AT_BALANCES);
            bodies.push(body);
        }
    }
    bodies.dedup();
    assert_eq!(bodies.len(), 1, "one body for all of them");
}

/// A query refused as malformed is refused before anything is read, for every token alike, so
/// no code's answer turns on its being in use.
#[tokio::test]
async fn a_malformed_fx_query_reads_the_same_in_use_or_not() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    let wide = held.world.token(Scope::Read, vec![all(Balances)]).await.1;
    for uri in [
        format!("/api/v1/fx/rates?currency=ZZQ&{RATES_THIS_YEAR}"),
        format!("/api/v1/fx/rates?currency=jpy&{RATES_THIS_YEAR}"),
        "/api/v1/fx/rates?currency=JPY".to_owned(),
        "/api/v1/fx/rates?currency=JPY&from=2026-1-1&to=2026-12-31".to_owned(),
        format!("/api/v1/fx/rates?{RATES_THIS_YEAR}"),
        "/api/v1/fx/coverage?currency=JPY".to_owned(),
    ] {
        let answer = held.whole(&uri, &held.token).await;
        if uri == "/api/v1/fx/coverage?currency=JPY" {
            // Coverage takes no query: anything sent is ignored, and the token is refused.
            assert_eq!(answer.1, StatusCode::FORBIDDEN);
        } else {
            assert_eq!(answer.1, StatusCode::BAD_REQUEST, "{uri}");
            assert!(held.whole(&uri, &wide).await == answer, "byte-identical");
        }
        assert!(
            bare.whole(&uri, &bare.token).await == answer,
            "byte-identical"
        );
        assert!(!answer.3.contains("JPY") && !answer.3.contains("ZZQ"));
    }
}

/// A token at `all` at `balances` may see every account, and coverage holds no more than it
/// may see: every currency, what was fetched, and nothing of any account or amount.
#[tokio::test]
async fn all_at_balances_reads_coverage_and_rates_and_no_more() {
    let held = scene().await;
    let hidden = hide_yen(&held.world).await;
    held.world.fetch_failed("THB", FxFetchStatus::Failing).await;
    let wide = held.world.token(Scope::Read, vec![all(Balances)]).await.1;

    let coverage = held.whole(COVERAGE, &wide).await;
    assert!(
        coverage == held.whole(COVERAGE, &held.admin).await,
        "byte-identical"
    );
    let rates = held.whole(&rates_of("JPY"), &wide).await;
    assert!(
        rates == held.whole(&rates_of("JPY"), &held.admin).await,
        "byte-identical"
    );

    assert_eq!(
        parsed(&rates.3),
        json!({"currency": "JPY", "items": [{"on": "2026-10-01", "rate": "175.68", "manual": false}]})
    );
    let listed = parsed(&coverage.3);
    let codes: Vec<_> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["currency"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["JPY", "THB", "USD"]);
    for text in [&coverage.3, &rates.3] {
        assert!(!text.contains(HIDDEN_NAME));
        assert!(!text.contains(&HIDDEN_MINOR.to_string()));
        assert!(!text.contains(&hidden.to_string()));
    }
}

// The display currency.

/// The leak this suite found. A rate converts the moment the display currency names it, so an
/// admin token held to one account could set the display currency to a guess and read whether
/// its own row converted: an amount meant the guess's rates were held, so the household uses
/// or used that currency; `not_fetched_yet` meant not. It takes one request per guess, for
/// about thirty currencies, and puts the setting back afterwards.
#[tokio::test]
async fn a_restricted_admin_cannot_learn_a_currency_by_setting_it() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    held.world.fetch_failed("THB", FxFetchStatus::Failing).await;
    let (_, bare_token) = bare
        .world
        .token(Scope::Admin, vec![on(bare.visible, Write)])
        .await;
    let (_, held_token) = held
        .world
        .token(Scope::Admin, vec![on(held.visible, Write)])
        .await;

    for guess in ["JPY", "THB", "CHF"] {
        let patch = json!({"display_currency": guess});
        let answers = (
            bare.world
                .send(Method::PATCH, SETTINGS, &bare_token, Some(&patch), None)
                .await,
            held.world
                .send(Method::PATCH, SETTINGS, &held_token, Some(&patch), None)
                .await,
        );
        assert!(answers.0 == answers.1, "byte-identical answer: {guess}");
        assert!(
            bare.whole(RANGE, &bare_token).await == held.whole(RANGE, &held_token).await,
            "byte-identical report after: {guess}"
        );
    }
}

/// Changing it spans every account, so a token needs `all` at `balances`, and the refusal is
/// fixed by the request alone: the code, in use or not, or the current one.
#[tokio::test]
async fn changing_the_display_currency_needs_all_at_balances() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    let (_, token) = bare
        .world
        .token(Scope::Admin, vec![on(bare.visible, Write)])
        .await;
    let (_, other) = held
        .world
        .token(Scope::Admin, vec![on(held.visible, Write)])
        .await;

    for guess in ["USD", "JPY", "CHF"] {
        let patch = json!({"display_currency": guess, "default_horizon_months": 61});
        let answer = bare
            .world
            .send(Method::PATCH, SETTINGS, &token, Some(&patch), None)
            .await;
        assert_eq!(answer.0, StatusCode::FORBIDDEN);
        assert_eq!(parsed(&answer.1)["detail"], NEEDS_ALL_AT_BALANCES);
        assert!(
            held.world
                .send(Method::PATCH, SETTINGS, &other, Some(&patch), None)
                .await
                == answer,
            "byte-identical"
        );
    }
    let settings = bare.world.get(SETTINGS, &bare.admin).await.1;
    assert_eq!(settings["display_currency"], "USD", "nothing changed");
}

/// Its other settings say nothing of what is held, so `admin` is enough for them.
#[tokio::test]
async fn a_restricted_admin_keeps_the_other_settings() {
    let held = scene().await;
    let (_, token) = held
        .world
        .token(Scope::Admin, vec![on(held.visible, Write)])
        .await;

    for patch in [
        json!({"stale_after_days": 9}),
        json!({"stale_after_days": 9, "display_currency": null}),
    ] {
        let (status, _) = held
            .world
            .call(Method::PATCH, SETTINGS, &token, Some(patch))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}

/// Changing it answers settings, for a token that may, and no sum, no currency in use and no
/// sign of a rebuild, in the same words for an empty household and a full one.
#[tokio::test]
async fn an_admin_token_with_grants_gets_the_setting_not_the_sum() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    let mut answers = Vec::new();
    for scene in [&bare, &held] {
        let (_, token) = scene.world.token(Scope::Admin, vec![all(Balances)]).await;
        answers.push(
            scene
                .world
                .send(
                    Method::PATCH,
                    SETTINGS,
                    &token,
                    Some(&json!({"display_currency": "CHF"})),
                    None,
                )
                .await,
        );
    }

    assert_eq!(answers[0].0, StatusCode::OK);
    assert!(answers[0] == answers[1], "byte-identical");
    assert_eq!(
        parsed(&answers[0].1),
        json!({"emergency_funds": null, "include_income": false, "default_horizon_months": 3,
               "stale_after_days": 7, "display_currency": "CHF",
               "timezone": "UTC", "timezone_setting": null})
    );
}

/// `GET` says how numbers are shown to every token, and nothing else differs by token or by
/// what is hidden. `emergency_funds` is the one setting that needs more: none is set here, which
/// a token with the forecast's access reads as an empty list and any other as `null`.
#[tokio::test]
async fn settings_read_the_same_for_every_token() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    let wide = held.world.token(Scope::Read, vec![all(Balances)]).await.1;
    let mut tokens = restricted_tokens(&held).await;
    tokens.push(wide);
    let uniform = held.world.token(Scope::Admin, Vec::new()).await.1;

    let answers = for_each(&held, &tokens, SETTINGS).await;

    assert_eq!(parsed(&answers[0].3)["display_currency"], "USD");
    assert_eq!(parsed(&answers[0].3)["emergency_funds"], Value::Null);
    for answer in &answers {
        assert!(answer == &answers[0], "byte-identical");
    }
    assert!(
        bare.whole(SETTINGS, &bare.token).await == answers[0],
        "byte-identical"
    );
    let mut seen_by_all = parsed(&held.whole(SETTINGS, &uniform).await.3);
    assert_eq!(seen_by_all["emergency_funds"], json!([]));
    seen_by_all["emergency_funds"] = Value::Null;
    assert_eq!(seen_by_all, parsed(&answers[0].3), "nothing else differs");
}

// What a write token can try.

/// Creating an account needs `all` at `write`, so a token can't make one in a guessed currency
/// to see whether its rates are already held. The refusal doesn't depend on the guess.
#[tokio::test]
async fn a_write_token_cannot_create_a_probe_in_a_guessed_currency() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    let bare_token = bare
        .world
        .token(Scope::Write, vec![on(bare.visible, Write)])
        .await
        .1;
    let held_token = held
        .world
        .token(Scope::Write, vec![on(held.visible, Write)])
        .await
        .1;
    for guess in ["JPY", "THB", "USD"] {
        let body = json!({"name": "Probe", "kind": "checking", "currency": guess});
        let uri = "/api/v1/accounts";
        let answer = held
            .world
            .send(Method::POST, uri, &held_token, Some(&body), None)
            .await;
        assert_eq!(answer.0, StatusCode::FORBIDDEN);
        assert!(
            bare.world
                .send(Method::POST, uri, &bare_token, Some(&body), None)
                .await
                == answer,
            "byte-identical"
        );
    }
    let listed = held.world.get("/api/v1/accounts", &held_token).await.1;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "nothing made");
}

/// A token backdating its own account moves its own `first_on` and points, and its answers
/// still match a household with nothing hidden, whatever the hidden accounts hold that far
/// back.
#[tokio::test]
async fn a_back_dated_balance_of_its_own_finds_nothing_hidden() {
    let (bare, held) = (scene().await, scene().await);
    let old = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world.balance_on(old, "1970-01-01", HIDDEN_MINOR).await;
    hide_yen(&held.world).await;
    let mut answers = Vec::new();
    for scene in [&bare, &held] {
        let token = scene
            .world
            .token(Scope::Write, vec![on(scene.visible, Write)])
            .await
            .1;
        for on_day in ["1970-01-01", "1999-01-04", "2026-11-02", "2027-06-01"] {
            let uri = format!("/api/v1/accounts/{}/balances/{on_day}", scene.visible);
            let body = json!({"minor_units": 42});
            let (status, text) = scene
                .world
                .send(Method::PUT, &uri, &token, Some(&body), None)
                .await;
            assert_eq!(status, StatusCode::OK);
            answers.push(text);
        }
        for query in [
            RANGE,
            "/api/v1/reports/net-worth?from=1970-01-01&interval=month",
        ] {
            answers.push(scene.whole(query, &token).await.3);
        }
    }

    let (first, second) = answers.split_at(answers.len() / 2);
    assert!(first == second, "byte-identical");
    assert!(
        first[5].contains("\"first_on\":\"1970-01-01\""),
        "its own moved"
    );
}

/// A balance written to a hidden account answers like one written to a missing one, status and
/// bytes, valid or not.
#[tokio::test]
async fn a_balance_write_to_a_hidden_account_answers_like_a_missing_one() {
    let held = scene().await;
    let hidden = hide_yen(&held.world).await;
    let token = held
        .world
        .token(Scope::Write, vec![on(held.visible, Write)])
        .await
        .1;
    let missing = AccountId::new();
    let write = async |target: AccountId, body: &Value, on_day: &str| {
        let uri = format!("/api/v1/accounts/{target}/balances/{on_day}");
        held.world
            .send(Method::PUT, &uri, &token, Some(body), None)
            .await
    };

    for (body, on_day, status) in [
        (
            json!({"minor_units": 1}),
            "2026-10-26",
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"minor_units": 1}),
            "2026-13-99",
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"minor_units": "many"}),
            "2026-10-26",
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"unknown": true}),
            "2026-10-26",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let at_hidden = write(hidden, &body, on_day).await;
        assert_eq!(at_hidden.0, status, "{body} on {on_day}");
        assert!(
            at_hidden == write(missing, &body, on_day).await,
            "byte-identical"
        );
    }
    let (status, _) = held
        .world
        .get(&format!("/api/v1/accounts/{hidden}/balances"), &token)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let shown = held
        .world
        .get(&format!("/api/v1/accounts/{hidden}"), &token)
        .await;
    assert!(
        shown
            == held
                .world
                .get(&format!("/api/v1/accounts/{missing}"), &token)
                .await,
        "byte-identical"
    );
}

// The audit log.

/// The log holds a display change, a hidden balance and account, and the rest. A token below
/// `all` at `transactions` reads none of it, and can't tell by asking.
#[tokio::test]
async fn audit_reads_are_refused_alike_below_all_at_transactions() {
    let (bare, held) = (scene().await, scene().await);
    hide_yen(&held.world).await;
    held.world.display("EUR").await;
    let bare_tokens = {
        let mut tokens = restricted_tokens(&bare).await;
        tokens.push(bare.world.token(Scope::Read, vec![all(Balances)]).await.1);
        tokens
    };
    let held_tokens = {
        let mut tokens = restricted_tokens(&held).await;
        tokens.push(held.world.token(Scope::Read, vec![all(Balances)]).await.1);
        tokens
    };

    for uri in [
        "/api/v1/audit",
        "/api/v1/audit?entity_type=household",
        "/api/v1/audit?entity_type=account",
        "/api/v1/audit?entity_type=fx",
        "/api/v1/audit?limit=1",
    ] {
        let answers = for_each(&held, &held_tokens, uri).await;
        assert_identical(&for_each(&bare, &bare_tokens, uri).await, &answers);
        for (_, status, _, body) in answers {
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(
                parsed(&body)["detail"],
                "this action needs transactions access to every account"
            );
        }
    }
}

/// Rates and kept balances write no entry at all (N10), so a token that may read the log finds
/// nothing of a fetch, and a job's work can't be counted from it.
#[tokio::test]
async fn rates_and_kept_balances_leave_no_audit_entry() {
    let held = scene().await;
    let connection = held.world.connection(&[("Quiet unbound", None)]).await;
    let logged = held.world.audit_count().await;

    held.world.rates("JPY", &[("2026-10-01", "175.68")]).await;
    held.world.fetch_failed("THB", FxFetchStatus::Failing).await;
    keep_a_balance(&held.world, connection).await;

    assert_eq!(held.world.audit_count().await, logged);
}

async fn keep_a_balance(world: &World, connection: coffer_domain::ids::ConnectionId) {
    let listed = world
        .server
        .store
        .list_provider_accounts(Some(connection))
        .await
        .unwrap();
    let mut tx = world.server.store.begin().await.unwrap();
    tx.keep_balance(KeptBalance {
        provider_account: listed[0].id,
        on: day("2026-10-26"),
        balance_minor: HIDDEN_MINOR,
        currency: code("JPY"),
        recorded_at: world.server.clock.now(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// A balance kept for a provider account nobody has bound reaches no token's answer.
#[tokio::test]
async fn a_kept_balance_changes_no_byte() {
    let (bare, held) = (scene().await, scene().await);

    let connection = held.world.connection(&[("Quiet unbound", None)]).await;
    keep_a_balance(&held.world, connection).await;

    assert_same_for_the_token(&bare, &held).await;
}
