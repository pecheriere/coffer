//! The Plaid API client (<https://plaid.com/docs/api/>).
//!
//! Every request is a `POST` of a JSON body holding the client id and the
//! secret, through [`Egress::send`] to the configured environment's host and
//! no other. The credentials, tokens and response bodies stay out of errors
//! and logs: an error keeps Plaid's `error_type` and `error_code` only, once
//! they have the shape of a code, and drops everything else in the answer
//! (`error_message` and the request id included), because Plaid's free text
//! may echo what it was sent. This module logs nothing.
//!
//! Numbers are read as their exact JSON text, never through a float.

use std::fmt;

use coffer_domain::ids::MemberId;
use coffer_domain::money::DecimalAmount;
use jiff::civil::Date;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

use crate::config::PlaidConfig;
use crate::egress::{Egress, EgressError, HostAllowList, OutboundRequest};

const PLAID_VERSION: HeaderName = HeaderName::from_static("plaid-version");
const PLAID_VERSION_VALUE: HeaderValue = HeaderValue::from_static("2020-09-14");
const JSON: &str = "application/json";
const CLIENT_NAME: &str = "coffer";
const LANGUAGE: &str = "en";
const PRODUCTS: [&str; 1] = ["transactions"];
const DAYS_REQUESTED: u32 = 730;
/// The most updates `/transactions/sync` returns in one page.
const SYNC_PAGE_SIZE: u32 = 500;
/// The longest `error_type` or `error_code` kept.
const MAX_CODE_LEN: usize = 64;

const ITEM_LOGIN_REQUIRED: &str = "ITEM_LOGIN_REQUIRED";
const MUTATION_DURING_PAGINATION: &str = "TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION";
const RATE_LIMIT_EXCEEDED: &str = "RATE_LIMIT_EXCEEDED";
/// Stands for an `error_type` or `error_code` that is not shaped like a code.
const UNKNOWN_CODE: &str = "UNKNOWN";
/// The `error_type` of a refusal whose body is not one of Plaid's errors.
const UNREADABLE_TYPE: &str = "UNREADABLE";

const ANSWER_UNREADABLE: &str = "Plaid sent an answer coffer could not read";
const RECORD_UNREADABLE: &str = "Plaid sent a record coffer could not read";
const AMOUNT_UNREADABLE: &str = "Plaid sent an amount that is not an exact decimal";
const REQUEST_UNUSABLE: &str = "the Plaid request could not be built";

/// A failed Plaid request. No variant holds a credential, a token or a text
/// Plaid wrote freely.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlaidError {
    /// The Item needs its member to log in to the institution again.
    #[error("Plaid needs a new login to this institution")]
    ItemLoginRequired,
    /// Transactions changed while paging: restart from the first cursor.
    #[error("Plaid's data changed during pagination")]
    MutationDuringPagination,
    /// Plaid asks coffer to slow down.
    #[error("Plaid is rate limiting requests")]
    RateLimited,
    /// Plaid refused the request. Both values are `A-Z`, `0-9` and `_` only.
    /// A body that is not one of Plaid's errors is `UNREADABLE`, with
    /// `HTTP_<status>` as its code.
    #[error("Plaid refused the request: {error_type} {error_code}")]
    Api {
        /// Plaid's `error_type`.
        error_type: String,
        /// Plaid's `error_code`.
        error_code: String,
    },
    /// An answer, or a record in one, could not be read.
    #[error("{0}")]
    Invalid(&'static str),
    /// The request was refused or failed on the way out.
    #[error("Plaid could not be reached: {0}")]
    Egress(#[source] EgressError),
}

/// A linked Item: what exchanging a public token yields.
#[derive(Debug)]
pub struct PlaidItem {
    /// The credential for every later request about this Item.
    pub access_token: SecretString,
    /// Plaid's id of the Item.
    pub item_id: String,
}

/// An account of an Item.
#[derive(Debug, Clone)]
pub struct PlaidAccount {
    /// Plaid's id of the account.
    pub account_id: String,
    /// The name the institution or the member gave it.
    pub name: String,
    /// The institution's official name.
    pub official_name: Option<String>,
    /// Plaid's `type`, such as `depository` or `credit`.
    pub kind: String,
    /// Plaid's `subtype`, such as `checking`.
    pub subtype: Option<String>,
    /// The current balance as Plaid signs it: for a credit or loan account,
    /// positive is the amount owed.
    pub current: Option<DecimalAmount>,
    /// ISO 4217 currency, when the currency has a code.
    pub iso_currency_code: Option<String>,
    /// Plaid's name of a currency without an ISO 4217 code.
    pub unofficial_currency_code: Option<String>,
}

/// A transaction as Plaid reports it.
#[derive(Debug, Clone)]
pub struct PlaidTransaction {
    /// Plaid's id of the transaction.
    pub transaction_id: String,
    /// Plaid's id of its account.
    pub account_id: String,
    /// The amount as Plaid signs it: positive is money out.
    pub amount: DecimalAmount,
    /// ISO 4217 currency, when the currency has a code.
    pub iso_currency_code: Option<String>,
    /// Plaid's name of a currency without an ISO 4217 code.
    pub unofficial_currency_code: Option<String>,
    /// The posting date, or the expected one while pending.
    pub date: Date,
    /// The day the transaction was authorized.
    pub authorized_date: Option<Date>,
    /// The description.
    pub name: String,
    /// The merchant, when Plaid identified one.
    pub merchant_name: Option<String>,
    /// Whether it has not posted yet.
    pub pending: bool,
    /// The pending transaction this posted one replaces.
    pub pending_transaction_id: Option<String>,
}

/// One page of `/transactions/sync`. A record that cannot be read does not
/// fail the page: it is left out and listed in `unusable`.
#[derive(Debug, Clone)]
pub struct SyncPage {
    /// New transactions.
    pub added: Vec<PlaidTransaction>,
    /// Transactions that changed.
    pub modified: Vec<PlaidTransaction>,
    /// The ids of transactions that no longer exist.
    pub removed: Vec<String>,
    /// One entry per record of `added`, `modified` or `removed` that could
    /// not be read: its id, or `None` when not even that could be.
    pub unusable: Vec<Option<String>>,
    /// Where the next page, or the next sync, starts.
    pub next_cursor: String,
    /// Whether another page follows.
    pub has_more: bool,
}

/// The Plaid API, for one environment.
#[derive(Debug)]
pub struct PlaidClient {
    egress: Egress,
    config: PlaidConfig,
    base: Url,
    hosts: HostAllowList,
}

/// A request body: the credentials beside the endpoint's own fields.
#[derive(Serialize)]
struct PlaidRequest<'a, B> {
    client_id: &'a str,
    secret: &'a str,
    #[serde(flatten)]
    fields: B,
}

impl<B> fmt::Debug for PlaidRequest<'_, B> {
    /// Shows nothing: every field may be a credential or a token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaidRequest").finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct LinkTokenFields<'a> {
    client_name: &'static str,
    user: LinkUser,
    language: &'static str,
    country_codes: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    products: Option<[&'static str; 1]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transactions: Option<LinkTransactions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_token: Option<&'a str>,
}

#[derive(Serialize)]
struct LinkUser {
    client_user_id: String,
}

#[derive(Serialize)]
struct LinkTransactions {
    days_requested: u32,
}

#[derive(Serialize)]
struct PublicTokenFields<'a> {
    public_token: &'a str,
}

#[derive(Serialize)]
struct AccessTokenFields<'a> {
    access_token: &'a str,
}

#[derive(Serialize)]
struct SyncFields<'a> {
    access_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
    count: u32,
}

#[derive(Deserialize)]
struct LinkTokenAnswer {
    link_token: SecretString,
}

#[derive(Deserialize)]
struct ExchangeAnswer {
    access_token: SecretString,
    item_id: String,
}

#[derive(Deserialize)]
struct AccountsAnswer<'a> {
    #[serde(borrow)]
    accounts: Vec<RawAccount<'a>>,
}

#[derive(Deserialize)]
struct RawAccount<'a> {
    account_id: String,
    name: String,
    #[serde(default)]
    official_name: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(borrow)]
    balances: RawBalances<'a>,
}

#[derive(Deserialize)]
struct RawBalances<'a> {
    #[serde(default, borrow)]
    current: Option<&'a RawValue>,
    #[serde(default)]
    iso_currency_code: Option<String>,
    #[serde(default)]
    unofficial_currency_code: Option<String>,
}

/// A page, each record kept raw so that one malformed record does not spoil
/// the others.
#[derive(Deserialize)]
struct RawSyncPage<'a> {
    #[serde(borrow)]
    added: Vec<&'a RawValue>,
    #[serde(borrow)]
    modified: Vec<&'a RawValue>,
    #[serde(borrow)]
    removed: Vec<&'a RawValue>,
    next_cursor: String,
    has_more: bool,
}

#[derive(Deserialize)]
struct RawTransaction<'a> {
    transaction_id: String,
    account_id: String,
    #[serde(borrow)]
    amount: &'a RawValue,
    #[serde(default)]
    iso_currency_code: Option<String>,
    #[serde(default)]
    unofficial_currency_code: Option<String>,
    date: Date,
    #[serde(default)]
    authorized_date: Option<Date>,
    name: String,
    #[serde(default)]
    merchant_name: Option<String>,
    pending: bool,
    #[serde(default)]
    pending_transaction_id: Option<String>,
}

/// All that a removed record needs, and all that can still be read of a
/// malformed one.
#[derive(Deserialize)]
struct RecordId {
    transaction_id: String,
}

#[derive(Deserialize)]
struct RawError {
    error_type: String,
    error_code: String,
}

impl PlaidClient {
    /// A client for the environment `config` names, sending through `egress`
    /// to that environment's host only.
    ///
    /// # Errors
    ///
    /// [`PlaidError::Egress`] or [`PlaidError::Invalid`] if the host does
    /// not form an allow-list or a URL, which a known environment's does.
    pub fn new(egress: Egress, config: PlaidConfig) -> Result<Self, PlaidError> {
        let host = config.environment.host();
        let base = Url::parse(&format!("https://{host}/"))
            .map_err(|_| PlaidError::Invalid(REQUEST_UNUSABLE))?;
        let hosts = HostAllowList::parse(host).map_err(PlaidError::Egress)?;
        Ok(Self {
            egress,
            config,
            base,
            hosts,
        })
    }

    /// A client sending to a loopback mock instead of Plaid.
    ///
    /// # Panics
    ///
    /// When the mock's URL names no port, which it always does.
    #[cfg(feature = "testkit")]
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test helper: a mock without a port must abort the test using it"
    )]
    pub fn for_tests(mock: &crate::testkit::MockUpstream, config: PlaidConfig) -> Self {
        let port = mock.base.port().expect("the mock listens on a port");
        Self {
            egress: Egress::for_tests(),
            config,
            base: mock.base.clone(),
            hosts: HostAllowList::loopback(port),
        }
    }

    /// Creates a link token for `member`: for a new Item, or in update mode
    /// for the Item of the access token `update_for`.
    ///
    /// # Errors
    ///
    /// [`PlaidError`]: Plaid refused, the answer is unreadable, or the
    /// request failed on the way out.
    pub async fn link_token(
        &self,
        member: MemberId,
        update_for: Option<&SecretString>,
    ) -> Result<SecretString, PlaidError> {
        let new_item = update_for.is_none();
        let fields = LinkTokenFields {
            client_name: CLIENT_NAME,
            user: LinkUser {
                client_user_id: member.to_string(),
            },
            language: LANGUAGE,
            country_codes: &self.config.country_codes,
            products: new_item.then_some(PRODUCTS),
            transactions: new_item.then_some(LinkTransactions {
                days_requested: DAYS_REQUESTED,
            }),
            access_token: update_for.map(ExposeSecret::expose_secret),
        };
        let body = self.post("link/token/create", fields).await?;
        let answer: LinkTokenAnswer = read(&body)?;
        Ok(answer.link_token)
    }

    /// Exchanges the public token Link produced for an access token.
    ///
    /// # Errors
    ///
    /// As [`PlaidClient::link_token`].
    pub async fn exchange(&self, public_token: &SecretString) -> Result<PlaidItem, PlaidError> {
        let fields = PublicTokenFields {
            public_token: public_token.expose_secret(),
        };
        let body = self.post("item/public_token/exchange", fields).await?;
        let answer: ExchangeAnswer = read(&body)?;
        Ok(PlaidItem {
            access_token: answer.access_token,
            item_id: answer.item_id,
        })
    }

    /// The Item's accounts, with their balances.
    ///
    /// # Errors
    ///
    /// As [`PlaidClient::link_token`]; an account that cannot be read, a
    /// balance in exponent form for one, fails the whole answer.
    pub async fn accounts(
        &self,
        access_token: &SecretString,
    ) -> Result<Vec<PlaidAccount>, PlaidError> {
        let fields = AccessTokenFields {
            access_token: access_token.expose_secret(),
        };
        let body = self.post("accounts/get", fields).await?;
        let answer: AccountsAnswer<'_> = read(&body)?;
        answer
            .accounts
            .into_iter()
            .map(PlaidAccount::from_raw)
            .collect()
    }

    /// One page of transaction updates after `cursor`, or from the start of
    /// the Item's history without one.
    ///
    /// # Errors
    ///
    /// As [`PlaidClient::link_token`]. A record that cannot be read is no
    /// error: see [`SyncPage::unusable`].
    pub async fn sync_page(
        &self,
        access_token: &SecretString,
        cursor: Option<&str>,
    ) -> Result<SyncPage, PlaidError> {
        let fields = SyncFields {
            access_token: access_token.expose_secret(),
            cursor,
            count: SYNC_PAGE_SIZE,
        };
        let body = self.post("transactions/sync", fields).await?;
        let raw: RawSyncPage<'_> = read(&body)?;
        Ok(SyncPage::from_raw(raw))
    }

    /// Removes the Item at Plaid, which ends its access token and its
    /// billing.
    ///
    /// # Errors
    ///
    /// As [`PlaidClient::link_token`].
    pub async fn remove_item(&self, access_token: &SecretString) -> Result<(), PlaidError> {
        let fields = AccessTokenFields {
            access_token: access_token.expose_secret(),
        };
        self.post("item/remove", fields).await.map(drop)
    }

    /// Sends `fields` with the credentials to `path` and returns the body of
    /// a successful answer. Anything else becomes an error.
    async fn post<B: Serialize>(
        &self,
        path: &str,
        fields: B,
    ) -> Result<Zeroizing<Vec<u8>>, PlaidError> {
        let url = self
            .base
            .join(path)
            .map_err(|_| PlaidError::Invalid(REQUEST_UNUSABLE))?;
        let request = PlaidRequest {
            client_id: &self.config.client_id,
            secret: self.config.secret.expose_secret(),
            fields,
        };
        let body =
            serde_json::to_vec(&request).map_err(|_| PlaidError::Invalid(REQUEST_UNUSABLE))?;
        let response = self
            .egress
            .send(
                OutboundRequest {
                    method: Method::POST,
                    url,
                    basic_auth: None,
                    headers: vec![(PLAID_VERSION, PLAID_VERSION_VALUE)],
                    body: Some((JSON, body)),
                },
                &self.hosts,
            )
            .await
            .map_err(PlaidError::Egress)?;
        let body = Zeroizing::new(response.body);
        if response.status.is_success() {
            Ok(body)
        } else {
            Err(error_from(response.status, &body))
        }
    }
}

impl PlaidAccount {
    fn from_raw(raw: RawAccount<'_>) -> Result<Self, PlaidError> {
        let current = raw.balances.current.map(amount).transpose()?;
        Ok(Self {
            account_id: raw.account_id,
            name: raw.name,
            official_name: raw.official_name,
            kind: raw.kind,
            subtype: raw.subtype,
            current,
            iso_currency_code: raw.balances.iso_currency_code,
            unofficial_currency_code: raw.balances.unofficial_currency_code,
        })
    }
}

impl PlaidTransaction {
    /// Reads one record of `added` or `modified`.
    fn parse(record: &str) -> Result<Self, PlaidError> {
        let raw: RawTransaction<'_> =
            serde_json::from_str(record).map_err(|_| PlaidError::Invalid(RECORD_UNREADABLE))?;
        Ok(Self {
            amount: amount(raw.amount)?,
            transaction_id: raw.transaction_id,
            account_id: raw.account_id,
            iso_currency_code: raw.iso_currency_code,
            unofficial_currency_code: raw.unofficial_currency_code,
            date: raw.date,
            authorized_date: raw.authorized_date,
            name: raw.name,
            merchant_name: raw.merchant_name,
            pending: raw.pending,
            pending_transaction_id: raw.pending_transaction_id,
        })
    }
}

impl SyncPage {
    fn from_raw(raw: RawSyncPage<'_>) -> Self {
        let mut unusable = Vec::new();
        let added = transactions(&raw.added, &mut unusable);
        let modified = transactions(&raw.modified, &mut unusable);
        let mut removed = Vec::new();
        for record in &raw.removed {
            match record_id(record) {
                Some(id) => removed.push(id),
                None => unusable.push(None),
            }
        }
        Self {
            added,
            modified,
            removed,
            unusable,
            next_cursor: raw.next_cursor,
            has_more: raw.has_more,
        }
    }
}

/// The records that can be read; each of the others adds to `unusable`.
fn transactions(
    records: &[&RawValue],
    unusable: &mut Vec<Option<String>>,
) -> Vec<PlaidTransaction> {
    let mut read = Vec::new();
    for record in records {
        match PlaidTransaction::parse(record.get()) {
            Ok(transaction) => read.push(transaction),
            Err(_) => unusable.push(record_id(record)),
        }
    }
    read
}

fn record_id(record: &RawValue) -> Option<String> {
    serde_json::from_str::<RecordId>(record.get())
        .ok()
        .map(|record| record.transaction_id)
}

/// A JSON number as an exact decimal: a string, an exponent or any other
/// form is refused.
fn amount(raw: &RawValue) -> Result<DecimalAmount, PlaidError> {
    DecimalAmount::parse(raw.get()).map_err(|_| PlaidError::Invalid(AMOUNT_UNREADABLE))
}

/// Parses a successful answer. A parse error is dropped: its text can quote
/// the body.
fn read<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, PlaidError> {
    serde_json::from_slice(body).map_err(|_| PlaidError::Invalid(ANSWER_UNREADABLE))
}

/// The error an answer other than a success stands for.
fn error_from(status: StatusCode, body: &[u8]) -> PlaidError {
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS;
    let Ok(error) = serde_json::from_slice::<RawError>(body) else {
        return if rate_limited {
            PlaidError::RateLimited
        } else {
            PlaidError::Api {
                error_type: UNREADABLE_TYPE.to_owned(),
                error_code: format!("HTTP_{}", status.as_u16()),
            }
        };
    };
    match error.error_code.as_str() {
        ITEM_LOGIN_REQUIRED => PlaidError::ItemLoginRequired,
        MUTATION_DURING_PAGINATION => PlaidError::MutationDuringPagination,
        code if rate_limited
            || code == RATE_LIMIT_EXCEEDED
            || error.error_type == RATE_LIMIT_EXCEEDED =>
        {
            PlaidError::RateLimited
        }
        code => PlaidError::Api {
            error_type: code_or_unknown(&error.error_type),
            error_code: code_or_unknown(code),
        },
    }
}

/// `text` when shaped like one of Plaid's codes, else [`UNKNOWN_CODE`]: an
/// answer cannot put an echoed credential, which holds lowercase letters or
/// a hyphen, into an error this way.
fn code_or_unknown(text: &str) -> String {
    let is_code = (1..=MAX_CODE_LEN).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if is_code { text } else { UNKNOWN_CODE }.to_owned()
}

#[cfg(test)]
mod tests {
    use secrecy::{ExposeSecret as _, SecretString};

    use super::{
        AccessTokenFields, PlaidClient, PlaidError, PlaidRequest, PlaidTransaction, code_or_unknown,
    };
    use crate::config::{PlaidConfig, PlaidEnvironment};
    use crate::egress::{Egress, EgressError, HostAllowList};

    fn config(environment: PlaidEnvironment) -> PlaidConfig {
        PlaidConfig {
            client_id: "CLIENT-MARKER".to_owned(),
            secret: SecretString::from("SECRET-MARKER"),
            environment,
            country_codes: vec!["US".to_owned()],
        }
    }

    fn record(amount: &str) -> String {
        format!(
            r#"{{"transaction_id":"t","account_id":"a","amount":{amount},"date":"2027-01-10",
                "name":"n","pending":false}}"#
        )
    }

    #[test]
    fn amounts_parsed_from_raw_numbers() {
        for (raw, exact) in [
            ("12", "12"),
            ("4.5", "4.5"),
            ("-4.50", "-4.50"),
            ("0.1", "0.1"),
        ] {
            let parsed = PlaidTransaction::parse(&record(raw)).unwrap();
            assert_eq!(parsed.amount.as_str(), exact, "the exact text is kept");
        }
        for raw in [
            "1e2",
            "1E2",
            "1.5e-3",
            "\"12\"",
            "null",
            "true",
            "[12]",
            "1234567890123456789012",
        ] {
            assert!(
                matches!(
                    PlaidTransaction::parse(&record(raw)),
                    Err(PlaidError::Invalid(_))
                ),
                "only an exact decimal number is read"
            );
        }
    }

    #[test]
    fn other_fields_of_a_record_are_checked() {
        for bad in [
            r#"{"transaction_id":"t"}"#,
            r#"{"transaction_id":"t","account_id":"a","amount":1,"date":"not a date",
                "name":"n","pending":false}"#,
            r#"{"transaction_id":"t","account_id":"a","amount":1,"date":"2027-02-30",
                "name":"n","pending":false}"#,
            "7",
        ] {
            assert!(
                matches!(PlaidTransaction::parse(bad), Err(PlaidError::Invalid(_))),
                "a malformed record is invalid"
            );
        }
    }

    #[tokio::test]
    async fn only_the_environment_host_is_reachable() {
        let client = PlaidClient {
            egress: Egress::new(None).unwrap(),
            config: config(PlaidEnvironment::Sandbox),
            base: "https://attacker.example/".parse().unwrap(),
            hosts: HostAllowList::parse(PlaidEnvironment::Sandbox.host()).unwrap(),
        };
        let token = SecretString::from("access-token");
        assert!(
            matches!(
                client.accounts(&token).await,
                Err(PlaidError::Egress(EgressError::HostNotAllowed))
            ),
            "another host must be refused"
        );

        // Each environment's client allows its own host, not the other's.
        for (environment, own, other) in [
            (
                PlaidEnvironment::Sandbox,
                "https://sandbox.plaid.com/accounts/get",
                "https://production.plaid.com/accounts/get",
            ),
            (
                PlaidEnvironment::Production,
                "https://production.plaid.com/accounts/get",
                "https://sandbox.plaid.com/accounts/get",
            ),
        ] {
            let client = PlaidClient::new(Egress::new(None).unwrap(), config(environment)).unwrap();
            assert_eq!(client.base.host_str(), Some(environment.host()));
            assert!(
                client
                    .egress
                    .check_url(&own.parse().unwrap(), &client.hosts)
                    .is_ok()
            );
            assert!(matches!(
                client
                    .egress
                    .check_url(&other.parse().unwrap(), &client.hosts),
                Err(EgressError::HostNotAllowed)
            ));
        }
    }

    #[test]
    fn debug_output_redacts_credentials() {
        let config = config(PlaidEnvironment::Sandbox);
        let token = SecretString::from("TOKEN-MARKER");
        let request = PlaidRequest {
            client_id: &config.client_id,
            secret: config.secret.expose_secret(),
            fields: AccessTokenFields {
                access_token: token.expose_secret(),
            },
        };
        let client = PlaidClient::new(Egress::new(None).unwrap(), config.clone()).unwrap();
        for debug in [format!("{request:?}"), format!("{client:?}")] {
            for marker in ["CLIENT-MARKER", "SECRET-MARKER", "TOKEN-MARKER"] {
                assert!(
                    !debug.contains(marker),
                    "the Debug output must not show a credential"
                );
            }
        }
    }

    #[test]
    fn only_code_shaped_text_is_kept() {
        assert_eq!(code_or_unknown("INVALID_FIELD"), "INVALID_FIELD");
        assert_eq!(code_or_unknown("HTTP_502"), "HTTP_502");
        for bad in [
            "",
            "access-sandbox-1234",
            "lowercase",
            "WITH SPACE",
            "NEW\nLINE",
            &"A".repeat(65),
        ] {
            assert_eq!(code_or_unknown(bad), "UNKNOWN");
        }
    }
}
