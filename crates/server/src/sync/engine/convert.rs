//! Checking a provider account's data and converting its records.

use coffer_domain::money::Currency;
use coffer_store::{AccountRecord, ProviderAccountRecord, StoreTx};
use jiff::civil::Date;

use crate::sync::SyncError;
use crate::sync::apply::NormalizedTransaction;
use crate::sync::provider::{ProviderAccountData, ProviderTransaction, sanitize_provider_message};

const ACCOUNT_ERROR: &str = "the provider reported an error for this account";
const UNSUPPORTED_CURRENCY: &str =
    "the provider reported a currency that is not a supported ISO 4217 code";
const CURRENCY_DIFFERS: &str = "the provider reported a currency other than the bound account's";

/// Why an account's data cannot be applied.
pub(super) struct AccountRefusal {
    /// What to record on the provider account.
    pub(super) text: String,
    /// Fetching its records again cannot help: coffer cannot hold them.
    pub(super) lasting: bool,
}

impl AccountRefusal {
    pub(super) fn passing(text: String) -> Self {
        Self {
            text,
            lasting: false,
        }
    }
}

/// The bound account and the currency to convert amounts in, or why they
/// cannot be used.
pub(super) async fn check_account(
    tx: &mut StoreTx,
    record: &ProviderAccountRecord,
    data: &ProviderAccountData,
) -> Result<Result<(Option<AccountRecord>, Currency), AccountRefusal>, SyncError> {
    if let Some(error) = &data.error {
        let error = sanitize_provider_message(error);
        return Ok(Err(AccountRefusal::passing(if error.is_empty() {
            ACCOUNT_ERROR.to_owned()
        } else {
            error
        })));
    }
    let Ok(currency) = Currency::parse(&data.currency) else {
        return Ok(Err(AccountRefusal {
            text: UNSUPPORTED_CURRENCY.to_owned(),
            // No currency at all may come with the next answer; one coffer
            // does not support never becomes storable.
            lasting: !data.currency.is_empty(),
        }));
    };
    let account = match record.account {
        Some(id) => Some(tx.get_account(id).await?),
        None => None,
    };
    if account
        .as_ref()
        .is_some_and(|account| account.currency != currency)
    {
        return Ok(Err(AccountRefusal::passing(CURRENCY_DIFFERS.to_owned())));
    }
    Ok(Ok((account, currency)))
}

/// An account's records in its currency's minor units.
pub(super) struct Converted {
    pub(super) transactions: Vec<NormalizedTransaction>,
    /// The provider's unusable records, then the records whose amount does
    /// not convert exactly.
    pub(super) unusable: Vec<Option<String>>,
}

/// The account's records in `currency`'s minor units, a pending record
/// without a date dated `today`. A record whose amount does not convert
/// exactly, or a posted one without a date, joins `unusable`: unlike a
/// balance, which is a valuation, a ledger amount is never rounded.
pub(super) fn convert(
    records: Vec<ProviderTransaction>,
    mut unusable: Vec<Option<String>>,
    currency: Currency,
    today: Date,
) -> Converted {
    let mut transactions = Vec::with_capacity(records.len());
    for record in records {
        if record.posted_on.is_none() && !record.pending {
            unusable.push(Some(record.provider_id));
            continue;
        }
        let Ok(amount_minor) = record.amount.to_minor_units(currency) else {
            unusable.push(Some(record.provider_id));
            continue;
        };
        transactions.push(NormalizedTransaction {
            provider_id: record.provider_id,
            posted_on: record.posted_on.unwrap_or(today),
            date_reported: record.posted_on.is_some(),
            transacted_on: record.transacted_on,
            amount_minor,
            amount_raw: record.amount_raw,
            description: record.description,
            payee: record.payee,
            memo: record.memo,
            pending: record.pending,
            replaces_provider_id: record.replaces_provider_id,
        });
    }
    Converted {
        transactions,
        unusable,
    }
}
