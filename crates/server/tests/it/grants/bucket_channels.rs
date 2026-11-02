//! Spec §8.11 B6 and B7 under attack: a token's bucket is made from what it sees at `balances`,
//! and nothing else of the household reaches it. Each comparison is with a second household that
//! really lacks the hidden account, on the same clock and settings: the status, every header and
//! every byte of every read, with only the ids, which each household generates for itself,
//! named in the order they first appear. So an id that shouldn't be there still shows. Timing is
//! out of scope.

mod family;
mod hidden_cases;
mod more;
mod rates;
mod refusals;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use coffer_domain::accounts::{AccountKind, BelongsTo};
use coffer_domain::identity::Scope;
use coffer_domain::policy::{AccountLevel, TokenGrant};
use coffer_server::ledger::accounts::AccountPatch;
use serde_json::{Value, json};

use self::family::{
    BUCKET_MINOR, Family, HIDDEN_LABEL, HIDDEN_NAME, HOME_MINOR, LABEL, SINCE, Setup,
    assert_same_answers, assert_twins, family, family_with, ids_by_first_use,
};
use self::hidden_cases::{Apart, Beside, Hidden, changes_no_byte};
use super::net_worth_channels::{HIDDEN_MINOR, RANGE, TODAY, all, money};
use super::{on, world};

fn last_total(section: &Value) -> Value {
    section["points"].as_array().unwrap().last().unwrap()["total"].clone()
}

/// Its label, balance, currency, closing and age.
#[tokio::test]
async fn a_hidden_bucket_changes_no_byte() {
    for hidden in [
        Hidden::Apart(Apart::Label),
        Hidden::Beside(Beside::Balance),
        Hidden::Apart(Apart::Currency),
        Hidden::Beside(Beside::Closing),
        Hidden::Beside(Beside::Age),
    ] {
        changes_no_byte(hidden).await;
    }
}

/// While a rebuild is needed nobody gets a bucket, and hidden data doesn't change that.
#[tokio::test]
async fn a_hidden_bucket_reads_the_same_while_rebuilding() {
    let setup = Setup {
        built: false,
        ..Setup::default()
    };
    let (bare, held) = (
        family_with(setup, async |_| {}).await,
        family_with(setup, async |_| {}).await,
    );
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let report = held.report(&held.token).await;
    assert_eq!(report["blockers"], json!([{"kind": "rebuilding"}]));
    assert_eq!(report["buckets"], json!([]));
    assert_twins(&bare, &held).await;
}

/// No display currency: each section has the blocker, whoever else holds what.
#[tokio::test]
async fn a_hidden_bucket_reads_the_same_with_no_display_currency() {
    let setup = Setup {
        display: false,
        ..Setup::default()
    };
    let (bare, held) = (
        family_with(setup, async |_| {}).await,
        family_with(setup, async |_| {}).await,
    );
    let yen = held
        .world
        .bucket_account(
            HIDDEN_NAME,
            AccountKind::Education,
            "JPY",
            None,
            Some(LABEL),
        )
        .await;
    held.world.balance_on(yen, SINCE, HIDDEN_MINOR).await;
    let report = held.report(&held.token).await;
    assert_eq!(
        report["buckets"][0]["blockers"],
        json!([{"kind": "display_currency_not_set"}])
    );
    assert_twins(&bare, &held).await;
}

/// The sum is the visible one's, the answer identical to a household with only that one,
/// whichever of the two is older.
#[tokio::test]
async fn a_token_holding_one_of_two_sees_a_bucket_of_one() {
    for older in [false, true] {
        let bare = family().await;
        let held = family_with(Setup::default(), async |world| {
            if older {
                world.held_for(LABEL, HIDDEN_MINOR).await;
            }
        })
        .await;
        if !older {
            held.world.held_for(LABEL, HIDDEN_MINOR).await;
        }

        let report = held.report(&held.token).await;
        let buckets = report["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0]["beneficiary"], LABEL);
        assert_eq!(buckets[0]["status"], "firm");
        assert_eq!(buckets[0]["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(last_total(&buckets[0]), money(BUCKET_MINOR, "USD"));
        let whole = held.report(&held.admin).await;
        assert_eq!(whole["buckets"][0]["accounts"].as_array().unwrap().len(), 2);
        assert_eq!(
            last_total(&whole["buckets"][0]),
            money(BUCKET_MINOR + HIDDEN_MINOR, "USD")
        );
        assert_eq!(last_total(&report), last_total(&whole), "nothing moved");
        assert_twins(&bare, &held).await;
    }
}

/// One account to make, and whether the token holds it.
struct Entry {
    kind: AccountKind,
    label: Option<&'static str>,
    held: bool,
}

const fn seen(kind: AccountKind, label: Option<&'static str>) -> Entry {
    Entry {
        kind,
        label,
        held: true,
    }
}

const fn hidden(kind: AccountKind, label: Option<&'static str>) -> Entry {
    Entry {
        kind,
        label,
        held: false,
    }
}

/// A household of `entries` in order, those the token doesn't hold left out unless `whole`: the
/// report as the token gets it, ids named.
async fn report_for(entries: &[Entry], whole: bool) -> String {
    let world = world().await;
    world.on_today();
    world.display("USD").await;
    world.rebuild(TODAY).await;
    let mut grants: Vec<TokenGrant> = Vec::new();
    for (n, entry) in entries.iter().enumerate() {
        if !entry.held && !whole {
            continue;
        }
        let minor = if entry.held {
            i64::try_from(n).unwrap() + 1
        } else {
            HIDDEN_MINOR
        };
        let name = format!("Account {n}");
        let id = world
            .bucket_account(&name, entry.kind, "USD", None, entry.label)
            .await;
        world.balance_on(id, SINCE, minor).await;
        if entry.held {
            grants.push(on(id, AccountLevel::Balances));
        }
    }
    let (_, token) = world.token(Scope::Read, grants).await;
    ids_by_first_use(&world.get_whole(RANGE, &token).await.2)
}

/// A bucket's place comes from what it is alone, and an account's place in its bucket from the
/// accounts the token sees, whatever is hidden before, between or after them.
#[tokio::test]
async fn a_hidden_account_doesn_t_change_a_bucket_s_position() {
    use AccountKind::{Charitable, Custodial, Education};
    let cases = [
        vec![
            hidden(Education, Some("Aaron")),
            hidden(Charitable, Some("Aaron")),
            seen(Education, Some("Noah")),
            hidden(Education, Some("Mia")),
            seen(Education, Some("Emma")),
            hidden(Education, Some("Emma")),
            seen(Education, Some("Emma")),
            hidden(Education, Some("emma")),
            hidden(Education, Some("Emma ")),
            seen(Custodial, None),
            hidden(Custodial, None),
            seen(Charitable, Some("Emma")),
            hidden(Charitable, None),
            hidden(Education, Some("Zed")),
        ],
        vec![
            hidden(Custodial, None),
            hidden(Education, Some("Emma")),
            seen(Education, Some("Noah")),
            hidden(Charitable, Some("Zed")),
        ],
        // The same name in two normal forms is two labels.
        vec![
            seen(Education, Some("Ren\u{e9}e")),
            hidden(Education, Some("Rene\u{301}e")),
        ],
    ];
    let expected = [
        vec![
            ("someone_else", None),
            ("someone_else", Some("Emma")),
            ("someone_else", Some("Noah")),
            ("given_away", Some("Emma")),
        ],
        vec![("someone_else", Some("Noah"))],
        vec![("someone_else", Some("Ren\u{e9}e"))],
    ];
    for (entries, expected) in cases.iter().zip(expected) {
        let held = report_for(entries, true).await;
        let report: Value = serde_json::from_str(&held).unwrap();
        let places: Vec<(&str, Option<&str>)> = report["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|bucket| {
                (
                    bucket["belongs_to"].as_str().unwrap(),
                    bucket["beneficiary"].as_str(),
                )
            })
            .collect();
        assert_eq!(places, expected);
        assert_eq!(report_for(entries, false).await, held, "not byte-identical");
    }
}

/// The hidden account's rate state raises no caution on what the token sees, and makes neither
/// the bucket it would share nor the household provisional.
#[tokio::test]
async fn a_hidden_bucket_s_currency_raises_no_caution() {
    use coffer_domain::fx::FxFetchStatus;
    let cases: [(&str, &str, Option<FxFetchStatus>); 4] = [
        ("JPY", "not_fetched_yet", None),
        ("THB", "fetch_failing", Some(FxFetchStatus::Failing)),
        ("THB", "not_published", Some(FxFetchStatus::NotPublished)),
        ("JPY", "rate_stale", None),
    ];
    for (currency, effect, status) in cases {
        let (bare, held) = (family().await, family().await);
        let foreign = held
            .world
            .bucket_account(
                HIDDEN_NAME,
                AccountKind::Education,
                currency,
                None,
                Some(LABEL),
            )
            .await;
        held.world.balance_on(foreign, SINCE, HIDDEN_MINOR).await;
        if let Some(status) = status {
            held.world.fetch_failed(currency, status).await;
        }
        if effect == "rate_stale" {
            held.world.rates("JPY", &[("2026-01-02", "175.68")]).await;
            held.world.rates("USD", &[("2026-01-02", "1.1188")]).await;
        }

        let whole = held.report(&held.admin).await;
        assert!(whole["buckets"][0]["cautions"].to_string().contains(effect));
        assert_eq!(whole["buckets"][0]["status"], "provisional");
        let report = held.report(&held.token).await;
        assert_eq!(report["status"], "firm");
        assert_eq!(report["buckets"][0]["status"], "firm");
        assert_eq!(report["buckets"][0]["cautions"], json!([]));
        assert_twins(&bare, &held).await;
    }
}

/// A household full of other things, for a token holding one bucket account, reads as one where
/// that account is all there is, to a token holding it and to one with no restriction.
#[tokio::test]
async fn a_token_seeing_only_a_bucket_reads_like_a_household_with_only_that() {
    async fn lone(held_extras: bool) -> Family {
        let world = world().await;
        world.on_today();
        world.display("USD").await;
        world.rebuild(TODAY).await;
        if held_extras {
            let home = world.holding("Home", "USD").await;
            world.balance_on(home, SINCE, HOME_MINOR).await;
            world.held_for(LABEL, HIDDEN_MINOR).await;
            world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
            let yen = world.holding("Yen", "JPY").await;
            world.balance_on(yen, SINCE, HIDDEN_MINOR).await;
            world.rates("JPY", &[("2026-10-01", "175.68")]).await;
            world.rates("USD", &[("2026-10-01", "1.1188")]).await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let emma = world
            .bucket_account("Emma 529", AccountKind::Education, "USD", None, Some(LABEL))
            .await;
        world.balance_on(emma, SINCE, BUCKET_MINOR).await;
        let (_, token) = world
            .token(Scope::Read, vec![on(emma, AccountLevel::Balances)])
            .await;
        let (_, admin) = world.token(Scope::Admin, Vec::new()).await;
        Family {
            world,
            visible: vec![emma],
            token,
            admin,
        }
    }
    let (bare, held) = (lone(false).await, lone(true).await);

    let report = held.report(&held.token).await;
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["blockers"], json!([{"kind": "no_counted_accounts"}]));
    assert_eq!(report["accounts"], json!([]));
    assert_eq!(report["points"], json!([]));
    assert_eq!(report["buckets"].as_array().unwrap().len(), 1);
    assert_eq!(report["buckets"][0]["status"], "firm");
    assert_twins(&bare, &held).await;

    let (_, everything) = bare.world.token(Scope::Read, Vec::new()).await;
    for uri in [RANGE, "/api/v1/accounts"] {
        assert_eq!(
            bare.answer(uri, &everything).await,
            held.answer(uri, &held.token).await,
            "as a token with no restriction reads it"
        );
    }
    assert_eq!(
        last_total(&held.report(&held.admin).await["buckets"][0]),
        money(BUCKET_MINOR + HIDDEN_MINOR, "USD"),
        "the rest is really there"
    );
}

/// `balances` is all a bucket needs, and a report holds nothing of a transaction: its
/// description, amount or id, while the transactions themselves answer 403.
#[tokio::test]
async fn balances_level_is_enough_and_shows_no_transaction_data() {
    let (bare, busy) = (family().await, family().await);
    let transaction = busy
        .world
        .transaction(busy.emma(), 1, "Quiet tuition")
        .await;

    assert_twins(&bare, &busy).await;
    let text = busy.text(RANGE, &busy.token).await;
    assert_eq!(
        busy.report(&busy.token).await["buckets"][0]["status"],
        "firm"
    );
    assert!(text.contains(LABEL));
    assert!(!text.contains("Quiet tuition"), "no description");
    assert!(!text.contains(&transaction.to_string()), "no id");
    assert!(!text.contains("-1000"), "no amount");
    let (status, _) = busy
        .world
        .get(
            &format!("/api/v1/transactions?account={}", busy.emma()),
            &busy.token,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// One request is enough, both ways: the access comes from the grants as they are now, and the
/// answer is the one any token holding those grants gets.
#[tokio::test]
async fn a_grant_change_applies_at_the_next_request() {
    let held = family().await;
    let (id, token) = held
        .world
        .token(
            Scope::Read,
            vec![
                on(held.home(), AccountLevel::Balances),
                on(held.emma(), AccountLevel::Balances),
            ],
        )
        .await;
    assert!(held.text(RANGE, &token).await.contains(LABEL));

    let replace = async |grants: Vec<TokenGrant>| {
        held.world
            .server
            .agent_tokens
            .replace_grants(&held.world.alice, id, grants.clone())
            .await
            .unwrap();
        let (_, fresh) = held.world.token(Scope::Read, grants).await;
        fresh
    };

    let fresh = replace(vec![on(held.home(), AccountLevel::Balances)]).await;
    let narrowed = held.text(RANGE, &token).await;
    assert!(!narrowed.contains(LABEL), "the bucket has gone");
    assert_eq!(narrowed, held.text(RANGE, &fresh).await);
    let emma = format!("/api/v1/accounts/{}", held.emma());
    assert_eq!(held.world.get(&emma, &token).await.0, StatusCode::NOT_FOUND);
    assert!(!held.text("/api/v1/accounts", &token).await.contains(LABEL));

    let fresh = replace(vec![on(held.emma(), AccountLevel::Balances)]).await;
    let moved = held.text(RANGE, &token).await;
    assert!(moved.contains(LABEL) && !moved.contains("Home"));
    assert_eq!(moved, held.text(RANGE, &fresh).await);
}

/// An account created after a token is visible to it only through an `all` grant (§8.2), even in
/// a bucket it already holds part of.
#[tokio::test]
async fn a_new_bucket_account_needs_an_all_grant_to_show() {
    let held = family().await;
    let (_, wide) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Balances)])
        .await;
    let before = held.answers(&held.token).await;

    held.world.held_for(LABEL, HIDDEN_MINOR).await;
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;

    assert_same_answers(&before, &held.answers(&held.token).await);
    assert_eq!(
        held.text(RANGE, &wide).await,
        held.text(RANGE, &held.admin).await,
        "`all` at `balances` is the whole household's answer"
    );
    let report = held.report(&wide).await;
    assert_eq!(report["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(
        last_total(&report["buckets"][0]),
        money(BUCKET_MINOR + HIDDEN_MINOR, "USD")
    );
    assert_eq!(report["buckets"][1]["beneficiary"], HIDDEN_LABEL);
}

/// Passkey sessions have uniform access: every bucket, whatever grants exist.
#[tokio::test]
async fn a_session_sees_every_bucket() {
    let held = family().await;
    held.world.held_for(LABEL, HIDDEN_MINOR).await;
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;

    let report = held
        .world
        .server
        .ledger
        .net_worth(
            &held.world.alice,
            serde_json::from_value(json!({"from": SINCE, "to": TODAY, "interval": "day"})).unwrap(),
        )
        .await
        .unwrap();
    let report = serde_json::to_string(&report).unwrap();

    assert_eq!(
        report,
        held.text(RANGE, &held.admin).await,
        "the same answer"
    );
    assert!(report.contains(HIDDEN_LABEL) && report.contains(LABEL));
    assert_ne!(report, held.text(RANGE, &held.token).await);
}

fn roof() -> Value {
    json!({"items": [{
        "name": "Roof",
        "amount": {"minor_units": 100, "currency": "USD"},
        "direction": "outflow",
        "account_hint": null,
        "schedule": {"type": "one_off", "on": "2026-12-20"},
    }]})
}

/// R2: a partial forecast would be false, so a token with grants gets a 403, the same with or
/// without what it can't see, even holding `transactions` on all there is or `all` at `balances`.
#[tokio::test]
async fn a_forecast_stays_all_or_nothing() {
    let (bare, held) = (family().await, family().await);
    held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let mut answers = Vec::new();
    for scene in [&bare, &held] {
        let world = &scene.world;
        let named = vec![
            on(scene.home(), AccountLevel::Transactions),
            on(scene.emma(), AccountLevel::Transactions),
        ];
        let mut wide = named.clone();
        wide.push(all(AccountLevel::Balances));
        let mut these = Vec::new();
        for grants in [named, wide] {
            let (_, token) = world.token(Scope::Read, grants).await;
            let (status, body) = world
                .send(Method::GET, "/api/v1/forecast", &token, None, None)
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            these.push(body);
            let (status, body) = world
                .send(
                    Method::POST,
                    "/api/v1/forecast/what-if",
                    &token,
                    Some(&roof()),
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            these.push(body);
        }
        answers.push(these);
    }
    assert_eq!(answers[0], answers[1], "byte-identical");

    let (_, token) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Transactions)])
        .await;
    let (status, body) = held.world.get("/api/v1/forecast", &token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.to_string().contains("not_household"), "{body}");
}

/// R2, with B10: `not_the_households` sums accounts the token might not see, so it is answered
/// only with `all` at `transactions`.
#[tokio::test]
async fn plan_vs_actual_stays_all_or_nothing() {
    const URI: &str = "/api/v1/reports/plan-vs-actual?from=2026-10";
    let (bare, held) = (family().await, family().await);
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    let category = held.world.expense_category().await;
    bare.world.expense_category().await;
    let transaction = held.world.transaction(hidden, 1, "Quiet tuition").await;
    held.world.categorize(transaction, category).await;
    let mut answers = Vec::new();
    for scene in [&bare, &held] {
        let world = &scene.world;
        let named = vec![
            on(scene.home(), AccountLevel::Transactions),
            on(scene.emma(), AccountLevel::Transactions),
        ];
        let mut wide = named.clone();
        wide.push(all(AccountLevel::Balances));
        let mut these = Vec::new();
        for grants in [named, wide] {
            let (_, token) = world.token(Scope::Read, grants).await;
            let (status, _, body) = world.get_whole(URI, &token).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(!body.contains("not_the_households"));
            these.push(body);
        }
        answers.push(these);
    }
    assert_eq!(answers[0], answers[1], "byte-identical");

    let (_, token) = held
        .world
        .token(Scope::Read, vec![all(AccountLevel::Transactions)])
        .await;
    let (status, _, body) = held.world.get_whole(URI, &token).await;
    assert_eq!(status, StatusCode::OK);
    let report: Value = serde_json::from_str(&body).unwrap();
    assert!(
        report.to_string().contains("not_the_households") && report.to_string().contains("-1000"),
        "{report}"
    );
}

/// Patching the fields that make a bucket on a hidden account, whichever way, changes nothing
/// for the token, in one household before and after.
#[tokio::test]
async fn a_hidden_account_moving_between_buckets_changes_no_byte() {
    let held = family().await;
    let hidden = held.world.holding(HIDDEN_NAME, "USD").await;
    held.world.balance_on(hidden, SINCE, HIDDEN_MINOR).await;
    let before = held.answers(&held.token).await;

    let label = |text: &str| Some(text.to_owned());
    let moves = [
        AccountPatch {
            kind: Some(AccountKind::Education),
            ..AccountPatch::default()
        },
        AccountPatch {
            beneficiary: label(LABEL),
            ..AccountPatch::default()
        },
        AccountPatch {
            beneficiary: label(HIDDEN_LABEL),
            ..AccountPatch::default()
        },
        AccountPatch {
            closed: Some(true),
            ..AccountPatch::default()
        },
        AccountPatch {
            belongs_to: Some(BelongsTo::GivenAway),
            ..AccountPatch::default()
        },
        AccountPatch {
            closed: Some(false),
            beneficiary: Some(String::new()),
            ..AccountPatch::default()
        },
        AccountPatch {
            belongs_to: Some(BelongsTo::Household),
            ..AccountPatch::default()
        },
    ];
    for patch in moves {
        held.world.patch_account(hidden, patch).await;
        assert_same_answers(&before, &held.answers(&held.token).await);
    }
}

/// A change of the display currency rewrites every row, the hidden one's too, and a full rebuild
/// does again: the token's answers stay what they would be without it.
#[tokio::test]
async fn a_display_change_and_a_rebuild_beside_a_hidden_bucket_change_no_byte() {
    let (bare, held) = (family().await, family().await);
    held.world.held_for(LABEL, HIDDEN_MINOR).await;
    let yen = held
        .world
        .bucket_account(
            HIDDEN_NAME,
            AccountKind::Education,
            "JPY",
            None,
            Some(HIDDEN_LABEL),
        )
        .await;
    held.world.balance_on(yen, SINCE, HIDDEN_MINOR).await;
    for scene in [&bare, &held] {
        scene.world.rates("USD", &[("2026-10-25", "1.1188")]).await;
        scene.world.display("EUR").await;
    }
    assert_eq!(held.report(&held.token).await["display_currency"], "EUR");
    assert_twins(&bare, &held).await;
    for scene in [&bare, &held] {
        scene.world.rebuild(TODAY).await;
    }
    assert_twins(&bare, &held).await;
}

/// Binding a hidden bucket account to a provider account, which brings in balances kept while it
/// was unbound, moves nothing for a token that sees neither, in a household with the same
/// connection whose provider account stays unbound.
#[tokio::test]
async fn a_hidden_bucket_bound_to_a_provider_changes_no_byte() {
    let (bare, held) = (family().await, family().await);
    let hidden = held.world.held_for(HIDDEN_LABEL, HIDDEN_MINOR).await;
    for scene in [&bare, &held] {
        scene.world.connection(&[("Noah bank", None)]).await;
    }
    let provider = held
        .world
        .server
        .store
        .list_provider_accounts(None)
        .await
        .unwrap()[0]
        .id;
    held.world
        .server
        .sync
        .bind(&held.world.alice, provider, hidden)
        .await
        .unwrap();
    let linked = held
        .text(&format!("/api/v1/accounts/{hidden}"), &held.admin)
        .await;
    assert!(linked.contains("\"source\":\"linked\""), "{linked}");
    assert_twins(&bare, &held).await;
}
