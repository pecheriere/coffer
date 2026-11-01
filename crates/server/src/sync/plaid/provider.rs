//! Plaid behind the [`Provider`] interface.
//!
//! A sync reads the Item's accounts, then pages through
//! `/transactions/sync` from the saved cursor. Pages are netted: a record's
//! last state in the fetch is the one the engine sees, so a transaction added
//! on one page and removed on a later one is never stored. If Plaid's data
//! changes while paging, everything gathered is dropped and the fetch starts
//! again from the saved cursor.
//!
//! The credential is a small JSON document holding the access token, sealed
//! by the engine. Errors keep Plaid's codes at most (see [`super::client`]),
//! and nothing here logs.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use coffer_domain::accounts::AccountKind;
use coffer_domain::ids::MemberId;
use jiff::SignedDuration;
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};

use super::client::{
    PlaidAccount, PlaidAccounts, PlaidClient, PlaidError, PlaidTransaction, SyncPage,
    UnreadableAccount,
};
use crate::sync::apply::Coverage;
use crate::sync::provider::{
    Capabilities, ConnectInput, FetchMode, FetchRequest, FetchResult, Provider,
    ProviderAccountData, ProviderError, ProviderFuture, ProviderKind, ProviderTransaction,
    sanitize_provider_message,
};

/// How many times one fetch pages from the saved cursor before giving up on
/// data that keeps changing.
const MAX_PASSES: u32 = 3;
/// The most pages one pass reads: at 500 records a page, far more than a
/// household's history. It stops an upstream that never ends its answer.
const MAX_PAGES: usize = 200;
/// The most records one pass gathers, whatever the size of its pages.
const MAX_RECORDS: usize = 100_000;

/// Plaid's error type for a failure on its side.
const API_ERROR: &str = "API_ERROR";
/// Plaid's error type for an institution that is down or not answering.
const INSTITUTION_ERROR: &str = "INSTITUTION_ERROR";
/// The client's error type for a refusal whose body is not one of Plaid's.
const UNREADABLE: &str = "UNREADABLE";
/// Plaid's error codes for an Item or token that no longer exists.
const ITEM_GONE: [&str; 2] = ["ITEM_NOT_FOUND", "INVALID_ACCESS_TOKEN"];

const DATA_CHANGED: &str = "Plaid data changed during sync; try again";
const WRONG_INPUT: &str = "this connection is set up through Plaid Link";
const CREDENTIALS_UNUSABLE: &str =
    "the connection's Plaid credentials are not usable; reconnect it";
const CREDENTIALS_NOT_STORABLE: &str = "Plaid's access token could not be stored";
const TOO_MUCH_DATA: &str = "Plaid sent more records than one sync accepts";
const CURSOR_UNUSABLE: &str = "Plaid sent a sync cursor coffer could not store";
const ACCOUNT_UNREADABLE: &str = "Plaid sent this account in a form coffer could not read";
const ACCOUNT_NOT_LISTED: &str = "Plaid sent records for an account it did not list";
const ACCOUNT_WITHOUT_ID: &str = "Plaid sent an account without a usable id";
const RECORDS_WITHOUT_ACCOUNT: &str =
    "Plaid sent records coffer could not read or assign to an account";

/// The Plaid provider.
#[derive(Debug)]
pub struct Plaid {
    client: PlaidClient,
}

/// What is sealed as a Plaid connection's credentials.
#[derive(Serialize)]
struct NewCredentials<'a> {
    access_token: &'a str,
    item_id: &'a str,
}

/// The sealed credentials, opened. Its fields are private and it has no
/// `Debug`, so the token cannot reach a log by accident.
#[derive(Deserialize)]
struct Credentials {
    access_token: SecretString,
}

impl Credentials {
    /// A parse error is dropped: its text can quote the credentials.
    fn open(sealed: &SecretString) -> Result<Self, ProviderError> {
        serde_json::from_str(sealed.expose_secret())
            .map_err(|_| ProviderError::Invalid(CREDENTIALS_UNUSABLE.into()))
    }
}

/// What a record's last appearance in a fetch said about it.
enum NetState {
    Present(Box<PlaidTransaction>),
    Removed,
    Unusable,
}

/// A record's place in the fetch.
struct NetRecord {
    /// When it first appeared: a posted record keeps following the pending
    /// one it replaces, however often that one was modified after.
    first_seen: usize,
    account_id: String,
    state: NetState,
}

/// One pass over `/transactions/sync`, netted by transaction id.
#[derive(Default)]
struct Gathered {
    records: HashMap<String, NetRecord>,
    /// Unusable records without an id, by account.
    unnamed: BTreeMap<String, usize>,
    /// Whether some unusable record names no account to report it on.
    unattributed: bool,
    /// The records read so far, counting every appearance.
    total: usize,
    next: usize,
}

impl Gathered {
    fn record(&mut self, id: String, account_id: String, state: NetState) {
        let first_seen = self.next;
        self.next += 1;
        match self.records.entry(id) {
            Entry::Occupied(mut known) => {
                let known = known.get_mut();
                known.account_id = account_id;
                known.state = state;
            }
            Entry::Vacant(new) => {
                new.insert(NetRecord {
                    first_seen,
                    account_id,
                    state,
                });
            }
        }
    }

    /// Adds one page. Within a page, a changed record comes before a removal
    /// and an unreadable form last: the safest last word on a repeated id.
    fn add(&mut self, page: SyncPage) -> Result<(), PlaidError> {
        self.total = self
            .total
            .saturating_add(page.added.len())
            .saturating_add(page.modified.len())
            .saturating_add(page.removed.len())
            .saturating_add(page.unusable.len());
        if self.total > MAX_RECORDS {
            return Err(PlaidError::Invalid(TOO_MUCH_DATA));
        }
        for transaction in page.added.into_iter().chain(page.modified) {
            self.record(
                transaction.transaction_id.clone(),
                transaction.account_id.clone(),
                NetState::Present(Box::new(transaction)),
            );
        }
        for removed in page.removed {
            self.record(
                removed.transaction_id,
                removed.account_id,
                NetState::Removed,
            );
        }
        for unusable in page.unusable {
            match (unusable.transaction_id, unusable.account_id) {
                (Some(id), Some(account)) => self.record(id, account, NetState::Unusable),
                (None, Some(account)) => *self.unnamed.entry(account).or_default() += 1,
                (_, None) => self.unattributed = true,
            }
        }
        Ok(())
    }

    /// The netted records by account, each in the order of its first
    /// appearance.
    fn into_buckets(self) -> (BTreeMap<String, Bucket>, bool) {
        let mut records: Vec<(String, NetRecord)> = self.records.into_iter().collect();
        records.sort_by_key(|(_, record)| record.first_seen);
        let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
        for (id, record) in records {
            let bucket = buckets.entry(record.account_id).or_default();
            match record.state {
                NetState::Present(transaction) => bucket.transactions.push(*transaction),
                NetState::Removed => bucket.removed.push(id),
                NetState::Unusable => bucket.unusable.push(Some(id)),
            }
        }
        for (account, count) in self.unnamed {
            let bucket = buckets.entry(account).or_default();
            bucket.unusable.extend(std::iter::repeat_n(None, count));
        }
        (buckets, self.unattributed)
    }
}

/// One account's share of a fetch.
#[derive(Default)]
struct Bucket {
    transactions: Vec<PlaidTransaction>,
    removed: Vec<String>,
    unusable: Vec<Option<String>>,
}

impl Plaid {
    /// The provider, over `client`.
    #[must_use]
    pub fn new(client: PlaidClient) -> Self {
        Self { client }
    }

    async fn exchange(&self, public_token: &SecretString) -> Result<SecretString, ProviderError> {
        let item = self
            .client
            .exchange(public_token)
            .await
            .map_err(provider_error)?;
        let credentials = NewCredentials {
            access_token: item.access_token.expose_secret(),
            item_id: &item.item_id,
        };
        // The serialized text moves into the secret; an error is dropped
        // unread.
        serde_json::to_string(&credentials)
            .map(SecretString::from)
            .map_err(|_| ProviderError::Invalid(CREDENTIALS_NOT_STORABLE.into()))
    }

    async fn create_link_token(
        &self,
        member: MemberId,
        credentials: Option<&SecretString>,
    ) -> Result<SecretString, ProviderError> {
        let credentials = credentials.map(Credentials::open).transpose()?;
        self.client
            .link_token(member, credentials.as_ref().map(|open| &open.access_token))
            .await
            .map_err(provider_error)
    }

    async fn gather(
        &self,
        access_token: &SecretString,
        cursor: Option<&str>,
    ) -> Result<(Gathered, String), PlaidError> {
        let mut gathered = Gathered::default();
        let mut next = cursor.map(str::to_owned);
        for _ in 0..MAX_PAGES {
            let mut page = self
                .client
                .sync_page(access_token, next.as_deref().filter(|c| !c.is_empty()))
                .await?;
            let last = !page.has_more;
            let cursor = std::mem::take(&mut page.next_cursor);
            gathered.add(page)?;
            if last {
                return Ok((gathered, cursor));
            }
            next = Some(cursor);
        }
        Err(PlaidError::Invalid(TOO_MUCH_DATA))
    }

    /// Pages from `cursor`, starting over when Plaid's data changes during
    /// pagination; nothing of an abandoned pass is kept.
    async fn gather_with_restarts(
        &self,
        access_token: &SecretString,
        cursor: Option<&str>,
    ) -> Result<(Gathered, String), ProviderError> {
        for _ in 0..MAX_PASSES {
            match self.gather(access_token, cursor).await {
                Err(PlaidError::MutationDuringPagination) => {}
                other => return other.map_err(provider_error),
            }
        }
        Err(ProviderError::Unavailable(DATA_CHANGED.into()))
    }

    async fn fetch_item(&self, request: FetchRequest<'_>) -> Result<FetchResult, ProviderError> {
        let credentials = Credentials::open(request.credentials)?;
        let accounts = self
            .client
            .accounts(&credentials.access_token)
            .await
            .map_err(provider_error)?;
        let (gathered, cursor) = self
            .gather_with_restarts(&credentials.access_token, request.cursor)
            .await?;
        let cursor = storable_cursor(cursor)?;
        let (buckets, unattributed) = gathered.into_buckets();
        let (accounts, mut connection_errors) = account_data(accounts, buckets, request.today);
        if unattributed {
            connection_errors.push(RECORDS_WITHOUT_ACCOUNT.to_owned());
        }
        Ok(FetchResult {
            accounts,
            connection_errors,
            cursor,
        })
    }

    async fn remove_item(&self, credentials: &SecretString) -> Result<(), ProviderError> {
        // Credentials that do not parse cannot name an Item at Plaid: the
        // engine takes a refusal as nothing left to give up, as it does for
        // credentials it cannot open.
        let credentials =
            Credentials::open(credentials).map_err(|_| ProviderError::ReauthRequired)?;
        match self.client.remove_item(&credentials.access_token).await {
            Err(PlaidError::Api { error_code, .. }) if ITEM_GONE.contains(&error_code.as_str()) => {
                // Already gone at Plaid: nothing is left to give up, and
                // refusing would keep the connection forever.
                Ok(())
            }
            other => other.map_err(provider_error),
        }
    }
}

/// The cursor to save, `None` while Plaid has none to give (an Item whose
/// first update is not ready answers with an empty one). It is stored as it
/// came and sent back as is, so one that sanitizing would alter is refused
/// rather than stored altered.
fn storable_cursor(cursor: String) -> Result<Option<String>, ProviderError> {
    if cursor.is_empty() {
        return Ok(None);
    }
    if sanitize_provider_message(&cursor) != cursor {
        return Err(ProviderError::Invalid(CURSOR_UNUSABLE.into()));
    }
    Ok(Some(cursor))
}

/// Every account in the provider shape, and the errors about the connection
/// as a whole.
fn account_data(
    answer: PlaidAccounts,
    mut buckets: BTreeMap<String, Bucket>,
    today: jiff::civil::Date,
) -> (Vec<ProviderAccountData>, Vec<String>) {
    let mut accounts = Vec::new();
    let mut connection_errors = Vec::new();
    for account in answer.accounts {
        let bucket = buckets.remove(&account.account_id).unwrap_or_default();
        accounts.push(readable_account(account, bucket, today));
    }
    for account in answer.unreadable {
        let UnreadableAccount { account_id, name } = account;
        let Some(id) = account_id else {
            connection_errors.push(ACCOUNT_WITHOUT_ID.to_owned());
            continue;
        };
        let bucket = buckets.remove(&id).unwrap_or_default();
        accounts.push(failed_account(id, name, bucket, ACCOUNT_UNREADABLE));
    }
    for (id, bucket) in buckets {
        accounts.push(failed_account(id, None, bucket, ACCOUNT_NOT_LISTED));
    }
    (accounts, connection_errors)
}

/// A readable account and its records. A record in a currency other than its
/// account's cannot be stored as an amount of the account: it is unusable.
fn readable_account(
    account: PlaidAccount,
    bucket: Bucket,
    today: jiff::civil::Date,
) -> ProviderAccountData {
    let kind_hint = kind_hint(&account.kind, account.subtype.as_deref());
    let liability = matches!(account.kind.as_str(), "credit" | "loan");
    let balance = account.current.map(|current| {
        let signed = if liability {
            current.negated()
        } else {
            current
        };
        (today, signed)
    });
    let currency = account
        .iso_currency_code
        .or(account.unofficial_currency_code)
        .unwrap_or_default();
    let mut unusable = bucket.unusable;
    let mut transactions = Vec::new();
    for record in bucket.transactions {
        let record_currency = record
            .iso_currency_code
            .as_deref()
            .or(record.unofficial_currency_code.as_deref());
        if record_currency.is_some_and(|code| code != currency) {
            unusable.push(Some(record.transaction_id));
        } else {
            transactions.push(transaction(record));
        }
    }
    ProviderAccountData {
        provider_account_id: account.account_id,
        name: account.name,
        institution: None,
        currency,
        kind_hint,
        balance,
        transactions,
        removed: bucket.removed,
        coverage: Coverage::Delta,
        error: None,
        unusable,
    }
}

/// An account reported with `error`. Its records are carried along though the
/// engine ignores them: they tell it that this fetch holds data of the
/// account it could not store, so the cursor must not pass them by.
fn failed_account(
    id: String,
    name: Option<String>,
    bucket: Bucket,
    error: &str,
) -> ProviderAccountData {
    ProviderAccountData {
        name: name.unwrap_or_else(|| id.clone()),
        provider_account_id: id,
        institution: None,
        currency: String::new(),
        kind_hint: None,
        balance: None,
        transactions: bucket.transactions.into_iter().map(transaction).collect(),
        removed: bucket.removed,
        coverage: Coverage::Delta,
        error: Some(error.to_owned()),
        unusable: bucket.unusable,
    }
}

/// A record in the provider shape: Plaid's positive is money out, coffer's
/// negative.
fn transaction(record: PlaidTransaction) -> ProviderTransaction {
    ProviderTransaction {
        provider_id: record.transaction_id,
        posted_on: Some(record.date),
        transacted_on: record.authorized_date,
        amount: record.amount.negated(),
        description: sanitize_provider_message(&record.name),
        payee: record
            .merchant_name
            .as_deref()
            .map(sanitize_provider_message),
        memo: None,
        pending: record.pending,
        replaces_provider_id: record.pending_transaction_id,
    }
}

/// What Plaid's `type` and `subtype` suggest the account is.
fn kind_hint(kind: &str, subtype: Option<&str>) -> Option<AccountKind> {
    match (kind, subtype) {
        ("depository", Some("checking")) => Some(AccountKind::Checking),
        ("depository", Some("savings")) => Some(AccountKind::Savings),
        ("credit", _) => Some(AccountKind::CreditCard),
        ("loan", Some("mortgage")) => Some(AccountKind::Mortgage),
        ("loan", _) => Some(AccountKind::Loan),
        ("investment", Some("brokerage")) => Some(AccountKind::Brokerage),
        ("investment", Some("401k" | "403b" | "ira" | "roth" | "pension")) => {
            Some(AccountKind::Retirement)
        }
        _ => None,
    }
}

/// The provider error a client error stands for. A refusal on Plaid's side,
/// or an answer that is not one of Plaid's errors, is passing; any other is
/// Plaid refusing what it was sent.
fn provider_error(err: PlaidError) -> ProviderError {
    match err {
        PlaidError::ItemLoginRequired => ProviderError::ReauthRequired,
        PlaidError::RateLimited => ProviderError::RateLimited,
        PlaidError::MutationDuringPagination => ProviderError::Unavailable(DATA_CHANGED.into()),
        PlaidError::Api {
            status,
            ref error_type,
            ..
        } if status >= 500
            || [UNREADABLE, API_ERROR, INSTITUTION_ERROR].contains(&error_type.as_str()) =>
        {
            ProviderError::Unavailable(err.to_string())
        }
        PlaidError::Api { .. } => ProviderError::Invalid(err.to_string()),
        PlaidError::Invalid(text) => ProviderError::Invalid(text.to_owned()),
        PlaidError::Egress(err) => ProviderError::Egress(err),
    }
}

impl Provider for Plaid {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Plaid
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            pending: true,
            mode: FetchMode::Cursor,
            min_interval: SignedDuration::from_mins(15),
            min_scheduled_interval: SignedDuration::from_hours(1),
            daily_budget: 48,
        }
    }

    fn connect(
        &self,
        input: ConnectInput,
    ) -> ProviderFuture<'_, Result<SecretString, ProviderError>> {
        Box::pin(async move {
            match input {
                ConnectInput::PlaidPublicToken(token) => self.exchange(&token).await,
                ConnectInput::SimpleFinSetupToken(_) => {
                    Err(ProviderError::Invalid(WRONG_INPUT.into()))
                }
            }
        })
    }

    fn fetch<'a>(
        &'a self,
        request: FetchRequest<'a>,
    ) -> ProviderFuture<'a, Result<FetchResult, ProviderError>> {
        Box::pin(self.fetch_item(request))
    }

    /// Plaid bills an Item until it is removed, so a failure is returned and
    /// the member can retry.
    fn disconnect<'a>(
        &'a self,
        credentials: &'a SecretString,
    ) -> ProviderFuture<'a, Result<(), ProviderError>> {
        Box::pin(self.remove_item(credentials))
    }

    fn link_token<'a>(
        &'a self,
        member: MemberId,
        credentials: Option<&'a SecretString>,
    ) -> ProviderFuture<'a, Result<SecretString, ProviderError>> {
        Box::pin(self.create_link_token(member, credentials))
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::accounts::AccountKind;
    use coffer_domain::money::DecimalAmount;
    use jiff::civil::date;

    use super::{
        ACCOUNT_NOT_LISTED, ACCOUNT_UNREADABLE, ACCOUNT_WITHOUT_ID, Bucket, DATA_CHANGED, Gathered,
        MAX_RECORDS, account_data, kind_hint, provider_error, storable_cursor,
    };
    use crate::sync::plaid::client::{
        PlaidAccount, PlaidAccounts, PlaidError, PlaidTransaction, RemovedTransaction, SyncPage,
        UnreadableAccount, UnusableRecord,
    };
    use crate::sync::provider::ProviderError;

    fn api(status: u16, error_type: &str, error_code: &str) -> PlaidError {
        PlaidError::Api {
            status,
            error_type: error_type.to_owned(),
            error_code: error_code.to_owned(),
        }
    }

    fn account(id: &str, kind: &str, currency: Option<&str>, current: &str) -> PlaidAccount {
        PlaidAccount {
            account_id: id.to_owned(),
            name: format!("Account {id}"),
            official_name: None,
            kind: kind.to_owned(),
            subtype: None,
            current: Some(DecimalAmount::parse(current).unwrap()),
            iso_currency_code: currency.map(str::to_owned),
            unofficial_currency_code: None,
        }
    }

    fn record(id: &str, currency: Option<&str>) -> PlaidTransaction {
        PlaidTransaction {
            transaction_id: id.to_owned(),
            account_id: "a".to_owned(),
            amount: DecimalAmount::parse("1").unwrap(),
            iso_currency_code: currency.map(str::to_owned),
            unofficial_currency_code: None,
            date: date(2027, 1, 10),
            authorized_date: None,
            name: "n".to_owned(),
            merchant_name: None,
            pending: false,
            pending_transaction_id: None,
        }
    }

    fn sync_page(
        added: Vec<PlaidTransaction>,
        removed: &[&str],
        unusable: Vec<UnusableRecord>,
    ) -> SyncPage {
        SyncPage {
            added,
            modified: Vec::new(),
            removed: removed
                .iter()
                .map(|id| RemovedTransaction {
                    transaction_id: (*id).to_owned(),
                    account_id: "a".to_owned(),
                })
                .collect(),
            unusable,
            next_cursor: "c".to_owned(),
            has_more: false,
        }
    }

    fn unusable(id: Option<&str>, account: Option<&str>) -> UnusableRecord {
        UnusableRecord {
            transaction_id: id.map(str::to_owned),
            account_id: account.map(str::to_owned),
        }
    }

    /// A record's last state counts, in the place of its first appearance,
    /// so a posted record stays behind the pending one it replaces.
    #[test]
    fn pages_net_by_last_state_in_first_order() {
        let mut gathered = Gathered::default();
        gathered
            .add(sync_page(
                vec![record("first", None), record("second", None)],
                &["third"],
                Vec::new(),
            ))
            .unwrap();
        gathered
            .add(sync_page(
                vec![record("third", None), record("first", Some("USD"))],
                &["second"],
                vec![
                    unusable(Some("broken"), Some("a")),
                    unusable(None, Some("a")),
                    unusable(Some("no-account"), None),
                ],
            ))
            .unwrap();

        let (mut buckets, unattributed) = gathered.into_buckets();

        assert!(unattributed, "a record of no account is noted");
        let bucket = buckets.remove("a").unwrap();
        let present: Vec<_> = bucket
            .transactions
            .iter()
            .map(|record| record.transaction_id.as_str())
            .collect();
        assert_eq!(present, ["first", "third"]);
        assert_eq!(
            bucket.transactions[0].iso_currency_code.as_deref(),
            Some("USD"),
            "the later form replaces the earlier"
        );
        assert_eq!(bucket.removed, ["second"]);
        assert_eq!(bucket.unusable, [Some("broken".to_owned()), None]);
        assert!(buckets.is_empty());
    }

    #[test]
    fn too_many_records_are_refused() {
        let mut gathered = Gathered::default();
        let over = vec![unusable(None, None); MAX_RECORDS + 1];
        assert!(
            matches!(
                gathered.add(sync_page(Vec::new(), &[], over)),
                Err(PlaidError::Invalid(_))
            ),
            "more records than a sync accepts are refused"
        );
    }

    #[test]
    fn kind_hints() {
        for (kind, subtype, expected) in [
            ("depository", Some("checking"), Some(AccountKind::Checking)),
            ("depository", Some("savings"), Some(AccountKind::Savings)),
            ("depository", Some("cd"), None),
            ("depository", None, None),
            ("credit", Some("credit card"), Some(AccountKind::CreditCard)),
            ("credit", None, Some(AccountKind::CreditCard)),
            ("loan", Some("mortgage"), Some(AccountKind::Mortgage)),
            ("loan", Some("student"), Some(AccountKind::Loan)),
            ("loan", None, Some(AccountKind::Loan)),
            (
                "investment",
                Some("brokerage"),
                Some(AccountKind::Brokerage),
            ),
            ("investment", Some("401k"), Some(AccountKind::Retirement)),
            ("investment", Some("403b"), Some(AccountKind::Retirement)),
            ("investment", Some("ira"), Some(AccountKind::Retirement)),
            ("investment", Some("roth"), Some(AccountKind::Retirement)),
            ("investment", Some("pension"), Some(AccountKind::Retirement)),
            ("investment", Some("hsa"), None),
            ("other", None, None),
        ] {
            assert_eq!(kind_hint(kind, subtype), expected, "{kind} {subtype:?}");
        }
    }

    /// A passing failure is `Unavailable`, a refusal `Invalid`, and neither
    /// holds more than Plaid's codes.
    #[test]
    fn errors_map_by_who_is_at_fault() {
        for transient in [
            api(502, "UNREADABLE", "HTTP_502"),
            api(400, "UNREADABLE", "HTTP_400"),
            api(500, "API_ERROR", "INTERNAL_SERVER_ERROR"),
            api(503, "API_ERROR", "PLANNED_MAINTENANCE"),
            api(400, "INSTITUTION_ERROR", "INSTITUTION_DOWN"),
            api(500, "SOMETHING_NEW", "SOME_CODE"),
        ] {
            assert!(
                matches!(provider_error(transient), ProviderError::Unavailable(_)),
                "a failure on Plaid's side is passing"
            );
        }
        let refused = provider_error(api(400, "INVALID_INPUT", "INVALID_FIELD"));
        assert!(matches!(refused, ProviderError::Invalid(_)));
        assert_eq!(
            refused.record_text(),
            "Plaid refused the request: INVALID_INPUT INVALID_FIELD"
        );
        assert!(matches!(
            provider_error(PlaidError::ItemLoginRequired),
            ProviderError::ReauthRequired
        ));
        assert!(matches!(
            provider_error(PlaidError::RateLimited),
            ProviderError::RateLimited
        ));
        assert!(matches!(
            provider_error(PlaidError::Invalid("text")),
            ProviderError::Invalid(_)
        ));
        assert_eq!(
            provider_error(PlaidError::MutationDuringPagination).record_text(),
            DATA_CHANGED
        );
    }

    #[test]
    fn cursors_are_stored_only_unaltered() {
        assert_eq!(storable_cursor(String::new()).unwrap(), None);
        assert_eq!(
            storable_cursor("aGVsbG8+/==".to_owned())
                .unwrap()
                .as_deref(),
            Some("aGVsbG8+/==")
        );
        for bad in ["a\u{7}b", "https://u:p@h.example", &"c".repeat(501)] {
            assert!(
                matches!(
                    storable_cursor(bad.to_owned()),
                    Err(ProviderError::Invalid(_))
                ),
                "a cursor sanitizing would alter is refused"
            );
        }
    }

    /// Liabilities turn negative; other balances keep their sign; a record
    /// in another currency is unusable, and one that names none belongs to
    /// its account's.
    #[test]
    fn readable_accounts_are_mapped() {
        let answer = PlaidAccounts {
            accounts: vec![
                account("card", "credit", Some("USD"), "410"),
                account("loan", "loan", Some("USD"), "-5"),
                account("cash", "depository", Some("USD"), "110.5"),
            ],
            unreadable: Vec::new(),
        };
        let buckets = [(
            "cash".to_owned(),
            Bucket {
                transactions: vec![
                    record("same", Some("USD")),
                    record("unnamed", None),
                    record("foreign", Some("EUR")),
                ],
                removed: Vec::new(),
                unusable: Vec::new(),
            },
        )]
        .into_iter()
        .collect();
        let (accounts, errors) = account_data(answer, buckets, date(2027, 1, 15));
        assert_eq!(errors, Vec::<String>::new());
        let balance = |index: usize| {
            let (on, amount) = accounts[index].balance.as_ref().unwrap();
            assert_eq!(*on, date(2027, 1, 15));
            amount.as_str().to_owned()
        };
        assert_eq!(balance(0), "-410");
        assert_eq!(balance(1), "5");
        assert_eq!(balance(2), "110.5");
        let ids: Vec<_> = accounts[2]
            .transactions
            .iter()
            .map(|record| record.provider_id.as_str())
            .collect();
        assert_eq!(ids, ["same", "unnamed"]);
        assert_eq!(accounts[2].unusable, [Some("foreign".to_owned())]);
    }

    /// An unreadable account keeps its name; one without an id is reported
    /// on the connection; records of an account Plaid did not list become
    /// that account's error rather than disappearing.
    #[test]
    fn accounts_that_cannot_be_used_become_errors() {
        let answer = PlaidAccounts {
            accounts: Vec::new(),
            unreadable: vec![
                UnreadableAccount {
                    account_id: Some("bad".to_owned()),
                    name: Some("Bad".to_owned()),
                },
                UnreadableAccount {
                    account_id: None,
                    name: Some("no id".to_owned()),
                },
            ],
        };
        let buckets = [(
            "stranger".to_owned(),
            Bucket {
                transactions: Vec::new(),
                removed: vec!["gone".to_owned()],
                unusable: Vec::new(),
            },
        )]
        .into_iter()
        .collect();
        let (accounts, errors) = account_data(answer, buckets, date(2027, 1, 15));
        assert_eq!(errors, [ACCOUNT_WITHOUT_ID]);
        let summary: Vec<_> = accounts
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
            [
                ("bad", "Bad", Some(ACCOUNT_UNREADABLE)),
                ("stranger", "stranger", Some(ACCOUNT_NOT_LISTED)),
            ]
        );
        assert_eq!(accounts[1].removed, ["gone"]);
    }
}
