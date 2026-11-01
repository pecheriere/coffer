//! The `SimpleFin` Bridge provider (spec §6.2, protocol at
//! <https://www.simplefin.org/protocol.html>).
//!
//! A member pastes a setup token: the base64 of a one-time claim URL.
//! Claiming it (`POST`, empty body) returns the access URL, whose user
//! information is the credential; every fetch is one `GET
//! <access URL>/accounts` with that user information as basic auth.
//!
//! Both URLs must name a host of the configured allow-list (checked by
//! [`Egress::check_url`] before anything is sent, and again by
//! [`Egress::send`]), so a token cannot point coffer at an internal
//! address. The setup token, the claim URL, the access URL, its user
//! information and every response body stay out of errors and logs:
//! error texts are fixed, with at most a status code. Connection errors
//! the bridge reports in a readable answer are kept (the engine sanitizes
//! them), except any that holds the access URL's user or password.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coffer_domain::money::DecimalAmount;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use percent_encoding::percent_decode_str;
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::value::RawValue;
use url::Url;
use zeroize::Zeroizing;

use super::apply::Coverage;
use super::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider,
    ProviderAccountData, ProviderError, ProviderFuture, ProviderKind, ProviderTransaction,
};
use crate::egress::{Egress, EgressError, HostAllowList, OutboundRequest};

/// The longest setup token accepted, in bytes.
const MAX_SETUP_TOKEN_BYTES: usize = 2048;
/// The longest claim answer (the access URL) accepted, in bytes.
const MAX_ACCESS_URL_BYTES: usize = 2048;

const TOKEN_INVALID: &str = "the setup token is not a SimpleFin setup token";
const TOKEN_HOST_REFUSED: &str = "the setup token names a host coffer may not contact";
const TOKEN_REFUSED: &str = "SimpleFin refused the setup token: it may have been claimed already";
const CLAIM_ANSWER_INVALID: &str = "SimpleFin answered the claim with no usable access URL";
const ACCESS_URL_UNUSABLE: &str =
    "the connection's SimpleFin access URL is not usable; reconnect it";
const SUBSCRIPTION_INACTIVE: &str = "SimpleFin subscription inactive";
const ANSWER_UNREADABLE: &str = "SimpleFin sent an answer coffer could not read";
const ACCOUNT_UNREADABLE: &str = "SimpleFin sent this account in a form coffer could not read";
const ACCOUNT_WITHOUT_ID: &str = "SimpleFin sent an account without a usable id";
const ERROR_WITHHELD: &str = "SimpleFin reported an error that coffer does not show";
const NO_WINDOW: &str = "no window start was given";

/// The `SimpleFin` Bridge provider.
pub struct SimpleFin {
    egress: Egress,
    hosts: HostAllowList,
}

impl fmt::Debug for SimpleFin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimpleFin")
            .field("hosts", &self.hosts)
            .finish_non_exhaustive()
    }
}

/// An access URL taken apart: the URL without user information, and the
/// percent-decoded user and password for basic auth.
struct Access {
    url: Url,
    user: String,
    password: SecretString,
}

impl SimpleFin {
    /// A provider sending through `egress` to `hosts` only.
    #[must_use]
    pub fn new(egress: Egress, hosts: HostAllowList) -> Self {
        Self { egress, hosts }
    }

    /// Claims the setup token and returns the access URL.
    async fn claim(&self, token: &SecretString) -> Result<SecretString, ProviderError> {
        let claim = claim_url(token)?;
        self.egress
            .check_url(&claim, &self.hosts)
            .map_err(claim_egress_error)?;
        let response = self
            .egress
            .send_limited(
                OutboundRequest {
                    method: Method::POST,
                    url: claim,
                    basic_auth: None,
                    headers: Vec::new(),
                    body: None,
                },
                &self.hosts,
                MAX_ACCESS_URL_BYTES,
            )
            .await
            .map_err(claim_egress_error)?;
        let body = Zeroizing::new(response.body);
        claim_status(response.status)?;
        let text = std::str::from_utf8(&body)
            .map_err(|_| ProviderError::Invalid(CLAIM_ANSWER_INVALID.into()))?;
        let access = Url::parse(text.trim())
            .map_err(|_| ProviderError::Invalid(CLAIM_ANSWER_INVALID.into()))?;
        self.access(&access)
            .map_err(|_| ProviderError::Invalid(CLAIM_ANSWER_INVALID.into()))?;
        Ok(SecretString::from(access.as_str()))
    }

    /// Takes `url` apart, checking that it carries a user and a password,
    /// no query or fragment, and that it is an allowed destination once
    /// its user information is removed.
    fn access(&self, url: &Url) -> Result<Access, ProviderError> {
        let unusable = || ProviderError::Invalid(ACCESS_URL_UNUSABLE.into());
        let encoded_password = url.password().filter(|password| !password.is_empty());
        let Some(encoded_password) = encoded_password else {
            return Err(unusable());
        };
        if url.username().is_empty() || url.query().is_some() || url.fragment().is_some() {
            return Err(unusable());
        }
        let user = percent_decode_str(url.username())
            .decode_utf8()
            .map_err(|_| unusable())?
            .into_owned();
        let password = SecretString::from(
            percent_decode_str(encoded_password)
                .decode_utf8()
                .map_err(|_| unusable())?
                .into_owned(),
        );
        let mut bare = url.clone();
        bare.set_username("").map_err(|()| unusable())?;
        bare.set_password(None).map_err(|()| unusable())?;
        self.egress
            .check_url(&bare, &self.hosts)
            .map_err(|_| unusable())?;
        Ok(Access {
            url: bare,
            user,
            password,
        })
    }

    async fn fetch_accounts(
        &self,
        request: FetchRequest<'_>,
    ) -> Result<FetchResult, ProviderError> {
        let start = request
            .window_start
            .ok_or_else(|| ProviderError::Invalid(NO_WINDOW.into()))?;
        let start_seconds = start
            .to_zoned(request.timezone.clone())
            .map_err(|_| ProviderError::Invalid(NO_WINDOW.into()))?
            .timestamp()
            .as_second();
        let stored = Url::parse(request.credentials.expose_secret())
            .map_err(|_| ProviderError::Invalid(ACCESS_URL_UNUSABLE.into()))?;
        let access = self.access(&stored)?;
        drop(stored);
        let mut url = access.url;
        url.path_segments_mut()
            .map_err(|()| ProviderError::Invalid(ACCESS_URL_UNUSABLE.into()))?
            .pop_if_empty()
            .push("accounts");
        url.query_pairs_mut()
            .append_pair("start-date", &start_seconds.to_string())
            .append_pair("pending", "1");
        let response = self
            .egress
            .send(
                OutboundRequest {
                    method: Method::GET,
                    url,
                    basic_auth: Some((access.user.clone(), access.password.clone())),
                    headers: Vec::new(),
                    body: None,
                },
                &self.hosts,
            )
            .await
            .map_err(|err| match err {
                EgressError::HostNotAllowed | EgressError::NotHttps => {
                    ProviderError::Invalid(ACCESS_URL_UNUSABLE.into())
                }
                other => {
                    ProviderError::Unavailable(format!("SimpleFin could not be reached: {other}"))
                }
            })?;
        let body = Zeroizing::new(response.body);
        match response.status {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(ProviderError::ReauthRequired);
            }
            StatusCode::PAYMENT_REQUIRED => {
                return Err(ProviderError::Unavailable(SUBSCRIPTION_INACTIVE.into()));
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(ProviderError::RateLimited),
            status => {
                return Err(ProviderError::Unavailable(format!(
                    "SimpleFin answered with status {}",
                    status.as_u16()
                )));
            }
        }
        let parsed = parse_account_set(&body, &request, start)?;
        let secrets = [access.user.as_str(), access.password.expose_secret()];
        Ok(withhold_credentials(parsed, &secrets))
    }
}

/// The claim URL a setup token encodes.
fn claim_url(token: &SecretString) -> Result<Url, ProviderError> {
    let invalid = || ProviderError::Invalid(TOKEN_INVALID.into());
    let token = token.expose_secret().trim();
    if token.is_empty() || token.len() > MAX_SETUP_TOKEN_BYTES {
        return Err(invalid());
    }
    let decoded = Zeroizing::new(STANDARD.decode(token).map_err(|_| invalid())?);
    let text = std::str::from_utf8(&decoded).map_err(|_| invalid())?;
    Url::parse(text.trim()).map_err(|_| invalid())
}

/// A failed claim request: a refused destination or an oversized answer is
/// the token's fault; anything else is the bridge being unavailable.
fn claim_egress_error(err: EgressError) -> ProviderError {
    match err {
        EgressError::HostNotAllowed | EgressError::NotHttps => {
            ProviderError::Invalid(TOKEN_HOST_REFUSED.into())
        }
        // An oversized error page reads as its status; only an oversized
        // successful answer is an invalid claim answer.
        EgressError::TooLarge { status } => claim_status(status)
            .err()
            .unwrap_or_else(|| ProviderError::Invalid(CLAIM_ANSWER_INVALID.into())),
        other => ProviderError::Unavailable(format!("SimpleFin could not be reached: {other}")),
    }
}

/// The error a claim answered with `status` stands for, if any.
fn claim_status(status: StatusCode) -> Result<(), ProviderError> {
    match status {
        StatusCode::OK => Ok(()),
        StatusCode::FORBIDDEN => Err(ProviderError::Invalid(TOKEN_REFUSED.into())),
        status if status.is_server_error() => Err(ProviderError::Unavailable(format!(
            "SimpleFin answered the claim with status {}",
            status.as_u16()
        ))),
        status => Err(ProviderError::Invalid(format!(
            "SimpleFin answered the claim with status {}",
            status.as_u16()
        ))),
    }
}

/// `result` with every connection error that holds one of `secrets`
/// replaced by a fixed text.
fn withhold_credentials(mut result: FetchResult, secrets: &[&str]) -> FetchResult {
    for error in &mut result.connection_errors {
        if secrets
            .iter()
            .any(|secret| !secret.is_empty() && error.contains(secret))
        {
            ERROR_WITHHELD.clone_into(error);
        }
    }
    result
}

/// An account set (protocol §"Account Set"): errors beside the accounts,
/// each account kept raw so that one malformed account does not spoil the
/// others.
#[derive(Deserialize)]
struct RawAccountSet<'a> {
    #[serde(default)]
    errors: Vec<String>,
    #[serde(borrow)]
    accounts: Vec<&'a RawValue>,
}

/// An account, each transaction record kept raw so that one malformed
/// record does not spoil the others.
#[derive(Deserialize)]
struct RawAccount<'a> {
    id: String,
    name: String,
    currency: String,
    balance: String,
    #[serde(rename = "balance-date")]
    balance_date: i64,
    #[serde(default)]
    org: Option<RawOrg>,
    #[serde(default, borrow)]
    transactions: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct RawOrg {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawTransaction {
    id: String,
    posted: i64,
    amount: String,
    description: String,
    #[serde(default)]
    payee: Option<String>,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    transacted_at: Option<i64>,
    #[serde(default)]
    pending: Option<bool>,
}

/// What can still be read of a malformed account.
#[derive(Deserialize)]
struct AccountId {
    id: String,
}

/// What can still be read of a malformed transaction record.
#[derive(Deserialize)]
struct RecordId {
    id: String,
}

#[derive(Deserialize)]
struct AccountName {
    name: String,
}

/// Parses an account set answer.
fn parse_account_set(
    body: &[u8],
    request: &FetchRequest<'_>,
    start: Date,
) -> Result<FetchResult, ProviderError> {
    let set: RawAccountSet<'_> = serde_json::from_slice(body)
        .map_err(|_| ProviderError::Invalid(ANSWER_UNREADABLE.into()))?;
    let mut connection_errors = set.errors;
    let mut accounts = Vec::with_capacity(set.accounts.len());
    for raw in set.accounts {
        match account(raw.get(), request, start) {
            Some(account) => accounts.push(account),
            None => connection_errors.push(ACCOUNT_WITHOUT_ID.to_owned()),
        }
    }
    Ok(FetchResult {
        accounts,
        connection_errors,
        cursor: None,
    })
}

/// One account; a malformed one becomes that account's error, and `None`
/// when not even its id can be read.
fn account(raw: &str, request: &FetchRequest<'_>, start: Date) -> Option<ProviderAccountData> {
    let coverage = Coverage::Window { start };
    if let Ok(parsed) = serde_json::from_str::<RawAccount<'_>>(raw)
        && let Some(account) = convert_account(&parsed, request, coverage)
    {
        return Some(account);
    }
    let id = serde_json::from_str::<AccountId>(raw).ok()?.id;
    let name =
        serde_json::from_str::<AccountName>(raw).map_or_else(|_| id.clone(), |named| named.name);
    Some(ProviderAccountData {
        provider_account_id: id,
        name,
        institution: None,
        currency: String::new(),
        kind_hint: None,
        balance: None,
        transactions: Vec::new(),
        removed: Vec::new(),
        coverage,
        error: Some(ACCOUNT_UNREADABLE.to_owned()),
        unusable: Vec::new(),
    })
}

/// A well-formed account in the provider shape; `None` when its balance or
/// balance date is unusable. A transaction record that cannot be used is
/// left out and listed among the account's unusable records.
fn convert_account(
    raw: &RawAccount<'_>,
    request: &FetchRequest<'_>,
    coverage: Coverage,
) -> Option<ProviderAccountData> {
    let balance = DecimalAmount::parse(&raw.balance).ok()?;
    let balance_on = day(raw.balance_date, request.timezone)?;
    let mut transactions = Vec::with_capacity(raw.transactions.len());
    let mut unusable = Vec::new();
    for record in &raw.transactions {
        match serde_json::from_str::<RawTransaction>(record.get()) {
            Ok(parsed) => match transaction(&parsed, request) {
                Some(converted) => transactions.push(converted),
                None => unusable.push(Some(parsed.id)),
            },
            Err(_) => unusable.push(
                serde_json::from_str::<RecordId>(record.get())
                    .ok()
                    .map(|record| record.id),
            ),
        }
    }
    Some(ProviderAccountData {
        provider_account_id: raw.id.clone(),
        name: raw.name.clone(),
        institution: raw.org.as_ref().and_then(|org| org.name.clone()),
        currency: raw.currency.clone(),
        kind_hint: None,
        balance: Some((balance_on, balance)),
        transactions,
        removed: Vec::new(),
        coverage,
        error: None,
        unusable,
    })
}

/// One transaction record. `posted` 0 or `pending: true` marks a pending
/// one, dated by `transacted_at`, else `posted`, else not at all (the
/// engine then dates it).
fn transaction(raw: &RawTransaction, request: &FetchRequest<'_>) -> Option<ProviderTransaction> {
    let amount = DecimalAmount::parse(&raw.amount).ok()?;
    let transacted_on = match raw.transacted_at {
        Some(at) if at != 0 => Some(day(at, request.timezone)?),
        _ => None,
    };
    let posted_on = if raw.posted == 0 {
        None
    } else {
        Some(day(raw.posted, request.timezone)?)
    };
    let pending = posted_on.is_none() || raw.pending == Some(true);
    let date = if pending {
        transacted_on.or(posted_on)
    } else {
        Some(posted_on?)
    };
    Some(ProviderTransaction {
        provider_id: raw.id.clone(),
        posted_on: date,
        transacted_on,
        amount,
        description: raw.description.clone(),
        payee: raw.payee.clone(),
        memo: raw.memo.clone(),
        pending,
        replaces_provider_id: None,
    })
}

/// The calendar day of Unix time `seconds` in `timezone`.
fn day(seconds: i64, timezone: &TimeZone) -> Option<Date> {
    Some(
        Timestamp::from_second(seconds)
            .ok()?
            .to_zoned(timezone.clone())
            .date(),
    )
}

impl Provider for SimpleFin {
    fn kind(&self) -> ProviderKind {
        ProviderKind::SimpleFin
    }

    /// About 24 requests a day are allowed; every sync is one request.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            pending: true,
            mode: FetchMode::Window {
                max_days: 90,
                overlap_days: 14,
            },
            min_interval: jiff::SignedDuration::from_hours(1),
            min_scheduled_interval: jiff::SignedDuration::from_hours(2),
            daily_budget: 16,
        }
    }

    fn connect(
        &self,
        input: ConnectInput,
    ) -> ProviderFuture<'_, Result<SecretString, ProviderError>> {
        let ConnectInput::SimpleFinSetupToken(token) = input;
        Box::pin(async move { self.claim(&token).await })
    }

    fn fetch<'a>(
        &'a self,
        request: FetchRequest<'a>,
    ) -> ProviderFuture<'a, Result<FetchResult, ProviderError>> {
        Box::pin(self.fetch_accounts(request))
    }

    /// Access is revoked at the bridge by the member; nothing to call.
    fn disconnect<'a>(
        &'a self,
        _credentials: &'a SecretString,
    ) -> ProviderFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;
    use secrecy::SecretString;

    use super::{
        ACCOUNT_UNREADABLE, ACCOUNT_WITHOUT_ID, ERROR_WITHHELD, FetchRequest, FetchResult,
        claim_url, parse_account_set, withhold_credentials,
    };
    use crate::sync::provider::ProviderError;

    fn request<'a>(credentials: &'a SecretString, timezone: &'a TimeZone) -> FetchRequest<'a> {
        FetchRequest {
            credentials,
            window_start: Some(date(2027, 1, 1)),
            cursor: None,
            today: date(2027, 1, 15),
            timezone,
        }
    }

    fn parse(body: &str) -> Result<FetchResult, ProviderError> {
        let credentials = SecretString::from("unused");
        let utc = TimeZone::UTC;
        parse_account_set(
            body.as_bytes(),
            &request(&credentials, &utc),
            date(2027, 1, 1),
        )
    }

    #[test]
    fn setup_token_decodes_to_its_claim_url() {
        let token = SecretString::from("aHR0cHM6Ly9icmlkZ2UuZXhhbXBsZS9jbGFpbS94");
        assert_eq!(
            claim_url(&token).unwrap().as_str(),
            "https://bridge.example/claim/x"
        );
        for bad in ["", "not base64!", "//79", &"QUFB".repeat(700)] {
            assert!(matches!(
                claim_url(&SecretString::from(bad)),
                Err(ProviderError::Invalid(_))
            ));
        }
    }

    #[test]
    fn pending_dates_fall_back_in_order() {
        let result = parse(
            r#"{"accounts":[{"id":"a","name":"A","currency":"USD","balance":"0",
                "balance-date":1799992800,"transactions":[
                {"id":"t1","posted":0,"amount":"-1","description":"x","transacted_at":1799595000},
                {"id":"t2","posted":1799745300,"amount":"-1","description":"x","pending":true},
                {"id":"t3","posted":0,"amount":"-1","description":"x"},
                {"id":"t4","posted":1799745300,"amount":"-1","description":"x"}]}]}"#,
        )
        .unwrap();
        let records = &result.accounts[0].transactions;
        let dated: Vec<_> = records
            .iter()
            .map(|record| (record.pending, record.posted_on))
            .collect();
        assert_eq!(
            dated,
            vec![
                (true, Some(date(2027, 1, 10))),
                (true, Some(date(2027, 1, 12))),
                (true, None),
                (false, Some(date(2027, 1, 12))),
            ],
            "a pending record without any date is left undated"
        );
    }

    /// Unusable records are left out and listed by id, or without one when
    /// none can be read; the account keeps its usable records.
    #[test]
    fn unusable_records_listed_by_id() {
        let result = parse(
            r#"{"accounts":[{"id":"a","name":"A","currency":"USD","balance":"0",
                "balance-date":1799992800,"transactions":[
                {"id":"ok","posted":1799745300,"amount":"-1","description":"x"},
                {"id":"no-amount","posted":1799745300,"description":"x"},
                {"id":"bad-amount","posted":1799745300,"amount":"1e3","description":"x"},
                {"id":"bad-time","posted":-99999999999999,"amount":"-1","description":"x"},
                {"amount":"-1"},
                7]}]}"#,
        )
        .unwrap();
        let account = &result.accounts[0];
        assert_eq!(account.error, None);
        assert_eq!(account.transactions.len(), 1);
        assert_eq!(
            account.unusable,
            vec![
                Some("no-amount".to_owned()),
                Some("bad-amount".to_owned()),
                Some("bad-time".to_owned()),
                None,
                None,
            ]
        );
    }

    #[test]
    fn malformed_accounts_become_account_errors() {
        let result = parse(
            r#"{"errors":["bridge note"],"accounts":[
                {"id":"a","name":"A","currency":"USD","balance":"1.5e3","balance-date":0},
                {"id":"b","currency":"USD"},
                {"name":"no id"},
                {"id":"c","name":"C","currency":"USD","balance":"1","balance-date":0,
                 "unknown":{"nested":[1,2]}}]}"#,
        )
        .unwrap();
        let summary: Vec<_> = result
            .accounts
            .iter()
            .map(|account| {
                (
                    account.provider_account_id.as_str(),
                    account.name.as_str(),
                    account.error.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("a", "A", Some(ACCOUNT_UNREADABLE)),
                ("b", "b", Some(ACCOUNT_UNREADABLE)),
                ("c", "C", None),
            ]
        );
        assert_eq!(
            result.connection_errors,
            vec!["bridge note".to_owned(), ACCOUNT_WITHOUT_ID.to_owned()]
        );
    }

    #[test]
    fn unreadable_answers_are_fixed_errors() {
        for body in ["", "not json", r#"{"errors":[]}"#, r#"{"accounts":{}}"#] {
            let Err(ProviderError::Invalid(message)) = parse(body) else {
                panic!("an unreadable answer is invalid");
            };
            assert_eq!(message, super::ANSWER_UNREADABLE);
        }
    }

    #[test]
    fn errors_holding_credentials_are_withheld() {
        let result = FetchResult {
            accounts: Vec::new(),
            connection_errors: vec![
                "bad login for user-1".to_owned(),
                "password pw-2 rejected".to_owned(),
                "kept".to_owned(),
            ],
            cursor: None,
        };
        let withheld = withhold_credentials(result, &["user-1", "pw-2", ""]);
        assert_eq!(
            withheld.connection_errors,
            vec![ERROR_WITHHELD, ERROR_WITHHELD, "kept"]
        );
    }
}
