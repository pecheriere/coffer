//! Binding provider accounts to coffer accounts, or creating the account to bind.

use coffer_domain::accounts::BalanceSource;
use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::{AccountId, ProviderAccountId};
use coffer_domain::money::{Currency, DecimalAmount};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, Action, authorize, authorize_account, authorize_every_account,
};
use coffer_store::{BalanceSnapshot, ProviderAccountRecord, StoreTx};
use jiff::Timestamp;
use jiff::civil::Date;

use super::{LinkedAccountView, NewLinkedAccountInput, ProviderAccountView, audit, visible};
use crate::ledger::Ledger;
use crate::ledger::accounts::{AccountView, NewAccountInput, ValidNewAccount, parse_name};
use crate::sync::SyncError;
use crate::sync::apply::apply_held;
use crate::sync::engine::{SyncService, ensure_not_removed, write_balance};

const ALREADY_BOUND: &str = "the provider account is already bound";
const ACCOUNT_CLOSED: &str = "the account is closed";
const ACCOUNT_ALREADY_FED: &str =
    "another provider account of this connection is already bound to the account";
const KIND_REQUIRED: &str = "kind is required: the provider gives no kind for this account";
const NAME_REQUIRED: &str = "name is required: the provider gives no usable name for this account";
/// The longest account name, in characters.
const MAX_ACCOUNT_NAME_CHARS: usize = 100;

/// The default name of an account created for a provider account: its
/// name, cut to the longest account name.
fn default_account_name(provider_account: &ProviderAccountRecord) -> String {
    provider_account
        .name
        .chars()
        .take(MAX_ACCOUNT_NAME_CHARS)
        .collect()
}

impl SyncService {
    /// Binds the provider account to `account` and turns the records held
    /// while it was unbound into transactions, in one transaction.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without write access to the account and to
    /// every account, or without [`Action::ManageConnections`] (a hidden
    /// account answers like an unknown one); [`SyncError::NotFound`] for
    /// an unknown provider account or account; [`SyncError::Conflict`] when
    /// the provider account is already bound, its connection is removed or
    /// the account is closed; [`SyncError::CurrencyMismatch`] when the
    /// currencies differ; storage failures.
    pub async fn bind(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
        account: AccountId,
    ) -> Result<ProviderAccountView, SyncError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, account, AccountLevel::Write)?;
        authorize_every_account(actor, &access, AccountLevel::Write)?;
        authorize(actor, Action::ManageConnections)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let (view, _) = bind_in(
            &mut tx,
            now,
            self.today(),
            actor,
            &access,
            provider_account,
            account,
        )
        .await?;
        tx.commit().await?;
        tracing::info!(provider_account = %provider_account, account = %account, "provider account bound");
        Ok(view)
    }

    /// Creates an account for the provider account and binds it, in one
    /// transaction: kind from the input, else the provider's hint; name
    /// from the input, else the provider account's; currency the provider
    /// account's. The account is created exactly as
    /// [`Ledger::create_account`] creates one, audit included.
    ///
    /// # Errors
    ///
    /// [`SyncError::Denied`] without [`Action::WriteHousehold`],
    /// [`Action::ManageConnections`] or write access to every account;
    /// [`SyncError::NotFound`] for an unknown provider account;
    /// [`SyncError::Invalid`] without a kind or for an invalid name;
    /// [`SyncError::CurrencyMismatch`] when the provider's currency is not a
    /// supported ISO 4217 code; the errors of [`SyncService::bind`].
    pub async fn create_account_and_bind(
        &self,
        actor: &Actor,
        provider_account: ProviderAccountId,
        input: NewLinkedAccountInput,
    ) -> Result<LinkedAccountView, SyncError> {
        authorize(actor, Action::WriteHousehold)?;
        authorize(actor, Action::ManageConnections)?;
        if let Some(name) = &input.name {
            parse_name(name)?;
        }
        let access = self.access.resolve(actor).await?;
        authorize_every_account(actor, &access, AccountLevel::Write)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_provider_account(provider_account).await?;
        if !visible(&access, &current) {
            return Err(SyncError::NotFound);
        }
        if current.account.is_some() {
            return Err(SyncError::Conflict(ALREADY_BOUND));
        }
        let kind = input
            .kind
            .or(current.kind_hint)
            .ok_or(SyncError::Invalid(KIND_REQUIRED))?;
        let currency =
            Currency::parse(&current.currency_raw).map_err(|_| SyncError::CurrencyMismatch)?;
        let name = if let Some(name) = input.name {
            name
        } else {
            let name = default_account_name(&current);
            if parse_name(&name).is_err() {
                return Err(SyncError::Invalid(NAME_REQUIRED));
            }
            name
        };
        let valid = ValidNewAccount::parse(NewAccountInput {
            name,
            kind,
            currency,
            liquidity: input.liquidity,
            include_in_net_worth: input.include_in_net_worth,
            include_in_forecast: input.include_in_forecast,
            opening_balance: None,
        })?;
        let (record, _) = Ledger::insert_account(&mut tx, now, actor, valid).await?;
        authorize_account(actor, &access, record.id, AccountLevel::Write)?;
        let (bound, balance) = bind_in(
            &mut tx,
            now,
            self.today(),
            actor,
            &access,
            provider_account,
            record.id,
        )
        .await?;
        tx.commit().await?;
        tracing::info!(provider_account = %provider_account, account = %record.id, "account created and bound");
        Ok(LinkedAccountView {
            account: AccountView::new(&record, balance.as_ref(), true),
            provider_account: bound,
        })
    }
}

/// Binds `provider_account` to `account` inside `tx`, applies its held
/// records and gives the account the balance last reported, which it returns;
/// the caller has authorized the actor.
async fn bind_in(
    tx: &mut StoreTx,
    now: Timestamp,
    today: Date,
    actor: &Actor,
    access: &AccountAccess,
    provider_account: ProviderAccountId,
    account: AccountId,
) -> Result<(ProviderAccountView, Option<BalanceSnapshot>), SyncError> {
    let current = tx.get_provider_account(provider_account).await?;
    if !visible(access, &current) {
        return Err(SyncError::NotFound);
    }
    if current.account.is_some() {
        return Err(SyncError::Conflict(ALREADY_BOUND));
    }
    ensure_not_removed(tx, current.connection).await?;
    let target = tx.get_account(account).await?;
    if target.closed_at.is_some() {
        return Err(SyncError::Conflict(ACCOUNT_CLOSED));
    }
    // Two provider accounts of one connection feeding one account would
    // import every record twice, and matching never pairs records of the
    // same connection.
    if tx
        .connection_binding(current.connection, account)
        .await?
        .is_some()
    {
        return Err(SyncError::Conflict(ACCOUNT_ALREADY_FED));
    }
    let currency =
        Currency::parse(&current.currency_raw).map_err(|_| SyncError::CurrencyMismatch)?;
    if currency != target.currency {
        return Err(SyncError::CurrencyMismatch);
    }
    let bound = tx.bind_provider_account(provider_account, account).await?;
    // The balance last reported, so net worth has the account before the
    // next sync. One that does not convert was noted on the provider account.
    let balance = match &bound.last_balance {
        Some(reported) => DecimalAmount::parse(&reported.amount_raw)
            .and_then(|amount| amount.to_minor_units_rounded(currency))
            .ok()
            .map(|balance_minor| BalanceSnapshot {
                account,
                // Never a future day, which would hide later balances.
                on: reported.on.min(today),
                balance_minor,
                source: BalanceSource::Sync,
                recorded_at: now,
            }),
        None => None,
    };
    if let Some(snapshot) = &balance {
        write_balance(tx, now, account, snapshot.on, snapshot.balance_minor).await?;
    }
    let report = apply_held(tx, now, bound.clone(), target).await?;
    audit(
        tx,
        now,
        actor,
        "provider_account.bind",
        ("provider_account", provider_account.to_string()),
        AuditChanges::new()
            .set("account", account.to_string())
            .set("connection", current.connection.to_string()),
    )
    .await?;
    tracing::debug!(
        provider_account = %provider_account,
        created = report.created,
        attached = report.attached,
        "held records applied"
    );
    Ok((ProviderAccountView::from(bound), balance))
}
