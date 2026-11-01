//! The Plaid API client against a loopback mock: what every request
//! carries, how recorded generic fixtures read, how Plaid's errors map, and
//! credentials that never reach an error or a log.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use coffer_domain::ids::MemberId;
use coffer_server::config::{PlaidConfig, PlaidEnvironment};
use coffer_server::sync::plaid::client::{PlaidClient, PlaidError};
use coffer_server::testkit::{MockUpstream, capture_logs, captured_log_text, log_marker};
use jiff::civil::date;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};

const LINK_TOKEN: &str = include_str!("fixtures/plaid/link_token_create.json");
const EXCHANGE: &str = include_str!("fixtures/plaid/public_token_exchange.json");
const ACCOUNTS: &str = include_str!("fixtures/plaid/accounts_get.json");
const PAGE_1: &str = include_str!("fixtures/plaid/transactions_sync_page1.json");
const PAGE_2: &str = include_str!("fixtures/plaid/transactions_sync_page2.json");
const POSTED: &str = include_str!("fixtures/plaid/transactions_sync_posted.json");
const LOGIN_REQUIRED: &str = include_str!("fixtures/plaid/error_item_login_required.json");
const MUTATION: &str = include_str!("fixtures/plaid/error_mutation_during_pagination.json");
const ITEM_REMOVE: &str = include_str!("fixtures/plaid/item_remove.json");

/// Credentials recognisable in a text search.
const CLIENT_ID: &str = "CLIENTID-MARKER-5e1";
const SECRET: &str = "PLAIDSECRET-MARKER-9c2";
const ACCESS_TOKEN: &str = "access-sandbox-ACCESSTOKEN-MARKER-11";
const PUBLIC_TOKEN: &str = "public-sandbox-PUBLICTOKEN-MARKER-22";
const MARKERS: [&str; 4] = [
    "CLIENTID-MARKER",
    "PLAIDSECRET-MARKER",
    "ACCESSTOKEN-MARKER",
    "PUBLICTOKEN-MARKER",
];

/// One request the mock received.
#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    path: String,
    query: Option<String>,
    plaid_version: Option<String>,
    content_type: Option<String>,
    has_authorization: bool,
    body: Value,
}

#[derive(Clone, Default)]
struct Script {
    replies: Arc<Mutex<VecDeque<(StatusCode, String)>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Script {
    fn reply(&self, status: StatusCode, body: impl Into<String>) {
        self.replies
            .lock()
            .unwrap()
            .push_back((status, body.into()));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn answer(
    State(script): State<Script>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let text = |name: &str| {
        headers
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    script.seen.lock().unwrap().push(Seen {
        method,
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        plaid_version: text("plaid-version"),
        content_type: text(header::CONTENT_TYPE.as_str()),
        has_authorization: headers.contains_key(header::AUTHORIZATION),
        body: serde_json::from_str(&body).unwrap(),
    });
    let (status, reply) = script.replies.lock().unwrap().pop_front().unwrap();
    (status, [(header::CONTENT_TYPE, "application/json")], reply).into_response()
}

fn config() -> PlaidConfig {
    PlaidConfig {
        client_id: CLIENT_ID.to_owned(),
        secret: SecretString::from(SECRET),
        environment: PlaidEnvironment::Sandbox,
        country_codes: vec!["US".to_owned(), "CA".to_owned()],
    }
}

/// The client under test, its mock and the mock's script. The mock stops
/// when dropped, so the caller keeps it.
async fn client() -> (PlaidClient, MockUpstream, Script) {
    capture_logs();
    let script = Script::default();
    let router = Router::new().fallback(answer).with_state(script.clone());
    let mock = MockUpstream::start(router).await;
    (PlaidClient::for_tests(&mock, config()), mock, script)
}

fn access_token() -> SecretString {
    SecretString::from(ACCESS_TOKEN)
}

#[tokio::test]
async fn requests_carry_credentials_in_body_and_version_header() {
    let (client, _mock, script) = client().await;
    let token = access_token();

    script.reply(StatusCode::OK, LINK_TOKEN);
    client.link_token(MemberId::new(), None).await.unwrap();
    script.reply(StatusCode::OK, EXCHANGE);
    client
        .exchange(&SecretString::from(PUBLIC_TOKEN))
        .await
        .unwrap();
    script.reply(StatusCode::OK, ACCOUNTS);
    client.accounts(&token).await.unwrap();
    script.reply(StatusCode::OK, PAGE_1);
    client.sync_page(&token, None).await.unwrap();
    script.reply(StatusCode::OK, ITEM_REMOVE);
    client.remove_item(&token).await.unwrap();

    let seen = script.seen();
    let paths: Vec<_> = seen.iter().map(|request| request.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/link/token/create",
            "/item/public_token/exchange",
            "/accounts/get",
            "/transactions/sync",
            "/item/remove",
        ]
    );
    for request in &seen {
        assert_eq!(request.method, Method::POST, "every request is a POST");
        assert_eq!(request.query, None, "no request has a query");
        assert_eq!(
            request.plaid_version.as_deref(),
            Some("2020-09-14"),
            "every request names the API version"
        );
        assert_eq!(
            request.content_type.as_deref(),
            Some("application/json"),
            "every request is JSON"
        );
        assert!(
            !request.has_authorization,
            "credentials travel in the body only"
        );
        assert!(
            request.body["client_id"] == CLIENT_ID,
            "every request carries the client id"
        );
        assert!(
            request.body["secret"] == SECRET,
            "every request carries the secret"
        );
    }
    assert!(
        seen[1].body["public_token"] == PUBLIC_TOKEN,
        "the exchange carries the public token"
    );
    for index in [2, 3, 4] {
        assert!(
            seen[index].body["access_token"] == ACCESS_TOKEN,
            "the request carries the access token"
        );
    }
}

#[tokio::test]
async fn link_token_for_a_new_item_asks_for_transactions() {
    let (client, _mock, script) = client().await;
    let member = MemberId::new();
    script.reply(StatusCode::OK, LINK_TOKEN);

    let token = client.link_token(member, None).await.unwrap();

    assert!(
        token.expose_secret() == "link-sandbox-00000000-0000-4000-8000-000000000001",
        "the link token is read from the answer"
    );
    assert!(
        script.seen()[0].body
            == json!({
                "client_id": CLIENT_ID,
                "secret": SECRET,
                "client_name": "coffer",
                "user": { "client_user_id": member.to_string() },
                "language": "en",
                "country_codes": ["US", "CA"],
                "products": ["transactions"],
                "transactions": { "days_requested": 730 },
            }),
        "a new Item's link request asks for transactions and 730 days"
    );
}

#[tokio::test]
async fn link_token_in_update_mode_names_the_item_instead_of_products() {
    let (client, _mock, script) = client().await;
    let member = MemberId::new();
    script.reply(StatusCode::OK, LINK_TOKEN);

    client
        .link_token(member, Some(&access_token()))
        .await
        .unwrap();

    assert!(
        script.seen()[0].body
            == json!({
                "client_id": CLIENT_ID,
                "secret": SECRET,
                "client_name": "coffer",
                "user": { "client_user_id": member.to_string() },
                "language": "en",
                "country_codes": ["US", "CA"],
                "access_token": ACCESS_TOKEN,
            }),
        "an update-mode link request names the Item and no products"
    );
}

#[tokio::test]
async fn exchange_returns_the_access_token_and_item() {
    let (client, _mock, script) = client().await;
    script.reply(StatusCode::OK, EXCHANGE);

    let item = client
        .exchange(&SecretString::from(PUBLIC_TOKEN))
        .await
        .unwrap();

    assert!(
        item.access_token.expose_secret() == "access-sandbox-00000000-0000-4000-8000-000000000002",
        "the access token is read from the answer"
    );
    assert_eq!(item.item_id, "fixtureItem0001");
    assert!(
        !format!("{item:?}").contains("access-sandbox"),
        "the access token must not appear in Debug output"
    );
}

#[tokio::test]
async fn accounts_read_from_the_fixture() {
    let (client, _mock, script) = client().await;
    script.reply(StatusCode::OK, ACCOUNTS);

    let accounts = client.accounts(&access_token()).await.unwrap();

    assert_eq!(accounts.len(), 3);
    let checking = &accounts[0];
    assert_eq!(checking.account_id, "acct-checking");
    assert_eq!(checking.name, "Plaid Checking");
    assert_eq!(
        checking.official_name.as_deref(),
        Some("First Platypus Bank Gold Standard Checking")
    );
    assert_eq!(checking.kind, "depository");
    assert_eq!(checking.subtype.as_deref(), Some("checking"));
    assert_eq!(checking.iso_currency_code.as_deref(), Some("USD"));
    let card = &accounts[1];
    assert_eq!(card.kind, "credit");
    assert_eq!(card.official_name, None);
    let crypto = &accounts[2];
    assert_eq!(crypto.subtype, None);
    assert_eq!(crypto.iso_currency_code, None);
    assert_eq!(crypto.unofficial_currency_code.as_deref(), Some("BTC"));
    assert!(crypto.current.is_none(), "a null balance reads as none");
}

#[tokio::test]
async fn sync_pages_read_from_the_fixtures() {
    let (client, _mock, script) = client().await;
    let token = access_token();

    script.reply(StatusCode::OK, PAGE_1);
    let first = client.sync_page(&token, None).await.unwrap();
    assert_eq!(first.next_cursor, "cursor-page-1");
    assert!(first.has_more);
    assert_eq!(first.added.len(), 2);
    let integer = &first.added[0];
    assert_eq!(integer.transaction_id, "txn-integer");
    assert_eq!(integer.account_id, "acct-checking");
    assert_eq!(integer.date, date(2027, 1, 10));
    assert_eq!(integer.authorized_date, Some(date(2027, 1, 9)));
    assert_eq!(integer.name, "CORNER COFFEE 0001");
    assert_eq!(integer.merchant_name.as_deref(), Some("Corner Coffee"));
    assert_eq!(integer.iso_currency_code.as_deref(), Some("USD"));
    assert_eq!(integer.unofficial_currency_code, None);
    assert!(!integer.pending);
    assert!(first.added[1].pending);

    script.reply(StatusCode::OK, PAGE_2);
    let second = client
        .sync_page(&token, Some("cursor-page-1"))
        .await
        .unwrap();
    assert_eq!(second.next_cursor, "cursor-page-2");
    assert!(!second.has_more);
    assert_eq!(second.added[0].amount.as_str(), "-1500.25");
    assert_eq!(second.modified[0].transaction_id, "txn-integer");
    assert_eq!(second.removed, ["txn-gone"]);
    assert_eq!(second.unusable, Vec::<Option<String>>::new());

    script.reply(StatusCode::OK, POSTED);
    let posted = client
        .sync_page(&token, Some("cursor-page-2"))
        .await
        .unwrap();
    assert_eq!(
        posted.added[0].pending_transaction_id.as_deref(),
        Some("txn-pending")
    );

    let seen = script.seen();
    assert_eq!(seen[0].body["count"], 500);
    assert!(
        seen[0].body.get("cursor").is_none(),
        "the first request has no cursor"
    );
    assert_eq!(seen[1].body["cursor"], "cursor-page-1");
    assert_eq!(seen[1].body["count"], 500);
}

#[tokio::test]
async fn amounts_read_exactly_from_fixtures() {
    let (client, _mock, script) = client().await;
    script.reply(StatusCode::OK, PAGE_1);
    script.reply(StatusCode::OK, ACCOUNTS);

    let page = client.sync_page(&access_token(), None).await.unwrap();
    assert_eq!(page.added[0].amount.as_str(), "12");
    assert_eq!(page.added[1].amount.as_str(), "4.5");

    let accounts = client.accounts(&access_token()).await.unwrap();
    assert_eq!(accounts[0].current.as_ref().unwrap().as_str(), "110.5");
    assert_eq!(accounts[1].current.as_ref().unwrap().as_str(), "410");
}

/// A record with a number coffer will not read as exact, or without a field
/// it needs, is left out and listed by id, or without one when none can be
/// read; the page keeps its usable records and its cursor.
#[tokio::test]
async fn one_bad_record_does_not_fail_the_page() {
    let (client, _mock, script) = client().await;
    let record = |id: &str, amount: &str| {
        format!(
            r#"{{"transaction_id":"{id}","account_id":"a","amount":{amount},"date":"2027-01-10",
                "name":"n","pending":false}}"#
        )
    };
    let body = format!(
        r#"{{"added":[{},{},{},{{"amount":1}},7],
             "modified":[{},{{"transaction_id":"no-fields"}}],
             "removed":[{{"transaction_id":"gone"}},{{"account_id":"a"}}],
             "next_cursor":"c","has_more":false}}"#,
        record("ok", "1.25"),
        record("exponent", "1e2"),
        record("quoted", "\"12\""),
        record("modified-ok", "3"),
    );
    script.reply(StatusCode::OK, body);

    let page = client.sync_page(&access_token(), None).await.unwrap();

    assert_eq!(page.added.len(), 1);
    assert_eq!(page.added[0].transaction_id, "ok");
    assert_eq!(page.modified.len(), 1);
    assert_eq!(page.modified[0].transaction_id, "modified-ok");
    assert_eq!(page.removed, ["gone"]);
    assert_eq!(page.next_cursor, "c");
    assert_eq!(
        page.unusable,
        vec![
            Some("exponent".to_owned()),
            Some("quoted".to_owned()),
            None,
            None,
            Some("no-fields".to_owned()),
            None,
        ]
    );
}

#[tokio::test]
async fn unreadable_answers_are_invalid() {
    let (client, _mock, script) = client().await;
    let token = access_token();
    for body in [
        "",
        "not json",
        r#"{"added":[]}"#,
        r#"{"added":{},"modified":[],"removed":[],"next_cursor":"c","has_more":false}"#,
    ] {
        script.reply(StatusCode::OK, body);
        let err = client.sync_page(&token, None).await.unwrap_err();
        assert!(
            matches!(err, PlaidError::Invalid(_)),
            "an unreadable page must be invalid"
        );
    }
    script.reply(
        StatusCode::OK,
        r#"{"accounts":[{"account_id":"a","name":"n","type":"other",
            "balances":{"current":1e2}}]}"#,
    );
    assert!(
        matches!(client.accounts(&token).await, Err(PlaidError::Invalid(_))),
        "a balance in exponent form must be invalid"
    );
    script.reply(StatusCode::OK, "{}");
    assert!(matches!(
        client.link_token(MemberId::new(), None).await,
        Err(PlaidError::Invalid(_))
    ));
    script.reply(StatusCode::OK, r#"{"item_id":"i"}"#);
    assert!(matches!(
        client.exchange(&SecretString::from(PUBLIC_TOKEN)).await,
        Err(PlaidError::Invalid(_))
    ));
}

#[tokio::test]
async fn error_body_mapped() {
    let (client, _mock, script) = client().await;
    let token = access_token();
    let error = |status: StatusCode, body: String| {
        let script = script.clone();
        let client = &client;
        let token = &token;
        async move {
            script.reply(status, body);
            client.sync_page(token, None).await.unwrap_err()
        }
    };

    assert!(matches!(
        error(StatusCode::BAD_REQUEST, LOGIN_REQUIRED.to_owned()).await,
        PlaidError::ItemLoginRequired
    ));
    assert!(matches!(
        error(StatusCode::BAD_REQUEST, MUTATION.to_owned()).await,
        PlaidError::MutationDuringPagination
    ));
    let rate_limited = json!({
        "error_type": "RATE_LIMIT_EXCEEDED",
        "error_code": "TRANSACTIONS_SYNC_LIMIT",
        "error_message": "rate limit exceeded",
    })
    .to_string();
    assert!(matches!(
        error(StatusCode::TOO_MANY_REQUESTS, rate_limited).await,
        PlaidError::RateLimited
    ));
    let rate_limit_code = json!({
        "error_type": "API_ERROR",
        "error_code": "RATE_LIMIT_EXCEEDED",
    })
    .to_string();
    assert!(matches!(
        error(StatusCode::BAD_REQUEST, rate_limit_code).await,
        PlaidError::RateLimited
    ));
    assert!(
        matches!(
            error(StatusCode::TOO_MANY_REQUESTS, String::new()).await,
            PlaidError::RateLimited
        ),
        "a 429 without a readable body is still a rate limit"
    );

    let other = json!({
        "error_type": "INVALID_INPUT",
        "error_code": "INVALID_FIELD",
        "error_message": "a message coffer does not keep",
    })
    .to_string();
    let err = error(StatusCode::BAD_REQUEST, other).await;
    let PlaidError::Api {
        error_type,
        error_code,
    } = &err
    else {
        panic!("an unrecognised error body maps to Api");
    };
    assert_eq!(error_type, "INVALID_INPUT");
    assert_eq!(error_code, "INVALID_FIELD");
    assert!(
        !err.to_string().contains("does not keep"),
        "the provider's message must not be kept"
    );

    let err = error(
        StatusCode::BAD_GATEWAY,
        "<html>bad gateway</html>".to_owned(),
    )
    .await;
    let PlaidError::Api {
        error_type,
        error_code,
    } = &err
    else {
        panic!("an unreadable error maps to Api");
    };
    assert_eq!(error_type, "UNREADABLE");
    assert_eq!(error_code, "HTTP_502");

    let err = error(StatusCode::FOUND, String::new()).await;
    assert!(
        matches!(&err, PlaidError::Api { error_code, .. } if error_code == "HTTP_302"),
        "a redirect is not followed and is not a success"
    );

    script.reply(StatusCode::INTERNAL_SERVER_ERROR, "");
    assert!(
        matches!(
            client.remove_item(&token).await,
            Err(PlaidError::Api { .. })
        ),
        "a failed removal must be reported"
    );
}

/// Every way a request can fail, each with an answer that echoes every
/// credential where a careless client would keep it: no error and no log
/// line shows them.
#[tokio::test]
async fn secrets_absent_from_errors_and_logs() {
    let (client, mock, script) = client().await;
    let token = access_token();
    let public_token = SecretString::from(PUBLIC_TOKEN);
    let marker = log_marker();

    let echoed = format!("{CLIENT_ID} {SECRET} {ACCESS_TOKEN} {PUBLIC_TOKEN}");
    let answers = [
        (
            StatusCode::BAD_REQUEST,
            json!({
                "error_type": echoed,
                "error_code": echoed,
                "error_message": echoed,
                "display_message": echoed,
                "request_id": echoed,
            })
            .to_string(),
        ),
        (
            StatusCode::BAD_REQUEST,
            json!({
                "error_type": ACCESS_TOKEN,
                "error_code": SECRET,
                "error_message": echoed,
            })
            .to_string(),
        ),
        (StatusCode::INTERNAL_SERVER_ERROR, echoed.clone()),
        (StatusCode::OK, echoed.clone()),
        (
            StatusCode::OK,
            json!({ "secret": echoed, "accounts": echoed, "link_token": 1, "added": echoed })
                .to_string(),
        ),
    ];

    let mut errors = Vec::new();
    for (status, body) in &answers {
        let reply = |script: &Script| script.reply(*status, body.clone());
        reply(&script);
        errors.push(client.link_token(MemberId::new(), None).await.unwrap_err());
        reply(&script);
        errors.push(
            client
                .link_token(MemberId::new(), Some(&token))
                .await
                .unwrap_err(),
        );
        reply(&script);
        errors.push(client.exchange(&public_token).await.unwrap_err());
        reply(&script);
        errors.push(client.accounts(&token).await.unwrap_err());
        reply(&script);
        errors.push(client.sync_page(&token, None).await.unwrap_err());
        if !status.is_success() {
            reply(&script);
            errors.push(client.remove_item(&token).await.unwrap_err());
        }
    }
    // A transport failure, once the mock has stopped.
    drop(mock);
    errors.push(client.accounts(&token).await.unwrap_err());
    assert!(
        matches!(errors.last(), Some(PlaidError::Egress(_))),
        "a stopped mock is an egress failure"
    );

    for err in &errors {
        for text in [err.to_string(), format!("{err:?}")] {
            for secret in MARKERS {
                assert!(
                    !text.contains(secret),
                    "an error must not show a credential"
                );
            }
        }
    }
    let debug = format!("{client:?}");
    for secret in MARKERS {
        assert!(
            !debug.contains(secret),
            "the client's Debug output must not show a credential"
        );
    }
    let log = captured_log_text();
    assert!(log.contains(&marker), "the capture still holds this test");
    for secret in MARKERS {
        assert!(!log.contains(secret), "a log must not show a credential");
    }
}
