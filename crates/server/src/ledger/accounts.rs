//! Accounts and their daily balance snapshots.

use coffer_domain::accounts::{
    AccountKind, AccountName, BalanceSource, LiquidityClass, default_liquidity,
};
use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::ids::AccountId;
use coffer_domain::money::{Currency, Money};
use coffer_domain::policy::{AccountLevel, Action, authorize, authorize_account};
use coffer_store::{AccountChanges, AccountRecord, BalanceSnapshot, NewAccount, PageRequest};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit, complete_created};
use crate::http::idempotency::IdempotencySlot;

const NAME_INVALID: &str = "account name must be 1 to 100 characters without control characters";

/// Where an account's data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    /// Kept by hand: balances (and transactions) are entered by members or
    /// agents, never by a sync.
    Manual,
}

/// An account's balance at the end of one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BalanceView {
    /// The calendar day (ISO 8601).
    pub on: Date,
    pub balance: Money,
    pub source: BalanceSource,
}

impl BalanceView {
    fn new(snapshot: &BalanceSnapshot, currency: Currency) -> Self {
        Self {
            on: snapshot.on,
            balance: Money {
                minor_units: snapshot.balance_minor,
                currency,
            },
            source: snapshot.source,
        }
    }
}

/// An account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the API contract: four independent flags an agent reads as JSON booleans"
)]
pub struct AccountView {
    pub id: AccountId,
    pub name: String,
    pub kind: AccountKind,
    /// Fixed when the account is created.
    pub currency: Currency,
    pub liquidity: LiquidityClass,
    /// Whether `liquidity` was chosen rather than defaulted from `kind`. A
    /// defaulted class follows a change of `kind`; a chosen one stays.
    pub liquidity_overridden: bool,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
    pub source: AccountSource,
    pub closed: bool,
    /// The most recent snapshot; `null` when there is none.
    #[schema(required)]
    pub latest_balance: Option<BalanceView>,
    pub created_at: Timestamp,
}

impl AccountView {
    fn new(record: &AccountRecord, latest: Option<&BalanceSnapshot>) -> Self {
        Self {
            id: record.id,
            name: record.name.as_str().to_owned(),
            kind: record.kind,
            currency: record.currency,
            liquidity: record.liquidity,
            liquidity_overridden: record.liquidity_overridden,
            include_in_net_worth: record.include_in_net_worth,
            include_in_forecast: record.include_in_forecast,
            source: AccountSource::Manual,
            closed: record.closed_at.is_some(),
            latest_balance: latest.map(|snapshot| BalanceView::new(snapshot, record.currency)),
            created_at: record.created_at,
        }
    }
}

/// One page of an account's balances, newest day first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BalancePage {
    pub items: Vec<BalanceView>,
    /// Pass as `after` for the next page; `null` on the last page.
    #[schema(required)]
    pub next: Option<String>,
}

/// A balance to record on the day an account is created from.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpeningBalance {
    /// The calendar day (ISO 8601).
    pub on: Date,
    /// The balance in the account currency's minor units; negative is owed.
    pub minor_units: i64,
}

/// An account to create.
#[derive(Debug, Clone)]
pub struct NewAccountInput {
    pub name: String,
    pub kind: AccountKind,
    pub currency: Currency,
    /// `None` takes the default class of `kind`.
    pub liquidity: Option<LiquidityClass>,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
    pub opening_balance: Option<OpeningBalance>,
}

/// Changes to an account; absent (or `null`) fields stay as they are. The
/// currency cannot change.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountPatch {
    /// 1 to 100 characters, without control characters.
    pub name: Option<String>,
    /// A new kind; a defaulted liquidity class follows it.
    pub kind: Option<AccountKind>,
    /// A chosen liquidity class, kept from then on whatever the kind.
    pub liquidity: Option<LiquidityClass>,
    pub include_in_net_worth: Option<bool>,
    pub include_in_forecast: Option<bool>,
    /// `true` closes the account, `false` reopens it.
    pub closed: Option<bool>,
}

fn parse_name(name: &str) -> Result<AccountName, LedgerError> {
    AccountName::parse(name).map_err(|_| LedgerError::Invalid(NAME_INVALID))
}

impl Ledger {
    /// Creates a manual account, with its opening balance if given. With
    /// `slot`, the idempotency key is completed with the response in the
    /// same transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::WriteHousehold`];
    /// [`LedgerError::Invalid`] for an invalid name; storage failures.
    pub async fn create_account(
        &self,
        actor: &Actor,
        input: NewAccountInput,
        slot: Option<IdempotencySlot>,
    ) -> Result<AccountView, LedgerError> {
        authorize(actor, Action::WriteHousehold)?;
        let name = parse_name(&input.name)?;
        let (liquidity, liquidity_overridden) = match input.liquidity {
            Some(chosen) => (chosen, true),
            None => (default_liquidity(input.kind), false),
        };
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let record = tx
            .insert_account(NewAccount {
                id: AccountId::new(),
                name,
                kind: input.kind,
                currency: input.currency,
                liquidity,
                liquidity_overridden,
                include_in_net_worth: input.include_in_net_worth,
                include_in_forecast: input.include_in_forecast,
                created_at: now,
            })
            .await?;
        let mut changes = AuditChanges::new()
            .set("name", record.name.as_str())
            .set("kind", record.kind.as_str())
            .set("currency", record.currency.as_str())
            .set("liquidity", record.liquidity.as_str())
            .set("liquidity_overridden", record.liquidity_overridden)
            .set("include_in_net_worth", record.include_in_net_worth)
            .set("include_in_forecast", record.include_in_forecast);
        let opening = match input.opening_balance {
            Some(opening) => {
                let snapshot = BalanceSnapshot {
                    account: record.id,
                    on: opening.on,
                    balance_minor: opening.minor_units,
                    source: BalanceSource::Manual,
                    recorded_at: now,
                };
                tx.upsert_balance(snapshot.clone()).await?;
                changes = changes
                    .set("opening_balance_on", snapshot.on)
                    .set("opening_balance_minor", snapshot.balance_minor);
                Some(snapshot)
            }
            None => None,
        };
        audit(
            &mut tx,
            now,
            actor,
            "account.create",
            "account",
            record.id,
            changes,
        )
        .await?;
        let view = AccountView::new(&record, opening.as_ref());
        complete_created(&mut tx, slot, &view).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Changes an account. Setting `kind` without `liquidity` re-defaults
    /// the liquidity class unless it was chosen. A patch that changes
    /// nothing writes nothing.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown or hidden account;
    /// [`LedgerError::Denied`] without write access to it;
    /// [`LedgerError::Invalid`] for an invalid name; storage failures.
    pub async fn update_account(
        &self,
        actor: &Actor,
        id: AccountId,
        patch: AccountPatch,
    ) -> Result<AccountView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, id, AccountLevel::Write)?;
        let name = patch.name.as_deref().map(parse_name).transpose()?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let current = tx.get_account(id).await?;

        let mut changes = AccountChanges::default();
        let mut audited = AuditChanges::new();
        if let Some(name) = name.filter(|name| *name != current.name) {
            audited = audited.change("name", current.name.as_str(), name.as_str());
            changes.name = Some(name);
        }
        let kind = patch.kind.unwrap_or(current.kind);
        if kind != current.kind {
            audited = audited.change("kind", current.kind.as_str(), kind.as_str());
            changes.kind = Some(kind);
        }
        let liquidity = match patch.liquidity {
            Some(chosen) => {
                if !current.liquidity_overridden {
                    audited = audited.change("liquidity_overridden", false, true);
                    changes.liquidity_overridden = Some(true);
                }
                chosen
            }
            None if current.liquidity_overridden => current.liquidity,
            None => default_liquidity(kind),
        };
        if liquidity != current.liquidity {
            audited = audited.change("liquidity", current.liquidity.as_str(), liquidity.as_str());
            changes.liquidity = Some(liquidity);
        }
        if let Some(include) = patch
            .include_in_net_worth
            .filter(|include| *include != current.include_in_net_worth)
        {
            audited = audited.change(
                "include_in_net_worth",
                current.include_in_net_worth,
                include,
            );
            changes.include_in_net_worth = Some(include);
        }
        if let Some(include) = patch
            .include_in_forecast
            .filter(|include| *include != current.include_in_forecast)
        {
            audited = audited.change("include_in_forecast", current.include_in_forecast, include);
            changes.include_in_forecast = Some(include);
        }
        let was_closed = current.closed_at.is_some();
        if let Some(closed) = patch.closed.filter(|closed| *closed != was_closed) {
            audited = audited.change("closed", was_closed, closed);
            changes.closed_at = Some(closed.then_some(now));
        }

        let record = if audited.is_empty() {
            drop(tx);
            current
        } else {
            let record = tx.update_account(id, changes).await?;
            audit(
                &mut tx,
                now,
                actor,
                "account.update",
                "account",
                id,
                audited,
            )
            .await?;
            tx.commit().await?;
            record
        };
        let latest = self.store.latest_balance(id).await?;
        Ok(AccountView::new(&record, latest.as_ref()))
    }

    /// One account with its latest balance.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown or hidden account; storage
    /// failures.
    pub async fn get_account(
        &self,
        actor: &Actor,
        id: AccountId,
    ) -> Result<AccountView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, id, AccountLevel::Balances)?;
        let record = self.store.get_account(id).await?;
        let latest = self.store.latest_balance(id).await?;
        Ok(AccountView::new(&record, latest.as_ref()))
    }

    /// The accounts the actor may see, oldest first; closed ones only when
    /// `include_closed`.
    ///
    /// # Errors
    ///
    /// Storage failures.
    pub async fn list_accounts(
        &self,
        actor: &Actor,
        include_closed: bool,
    ) -> Result<Vec<AccountView>, LedgerError> {
        let access = self.access.resolve(actor).await?;
        let scope = access.scope(AccountLevel::Balances);
        let records = self.store.list_accounts(&scope, include_closed).await?;
        let mut views = Vec::with_capacity(records.len());
        for record in &records {
            let latest = self.store.latest_balance(record.id).await?;
            views.push(AccountView::new(record, latest.as_ref()));
        }
        Ok(views)
    }

    /// Records a manual balance for `on`, replacing that day's snapshot
    /// whatever its source. Linked accounts accept one too; their next sync
    /// replaces the same day's snapshot in turn (the last write wins).
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown or hidden account;
    /// [`LedgerError::Denied`] without write access to it; storage failures.
    pub async fn set_manual_balance(
        &self,
        actor: &Actor,
        id: AccountId,
        on: Date,
        minor_units: i64,
    ) -> Result<BalanceView, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, id, AccountLevel::Write)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let account = tx.get_account(id).await?;
        let snapshot = BalanceSnapshot {
            account: id,
            on,
            balance_minor: minor_units,
            source: BalanceSource::Manual,
            recorded_at: now,
        };
        let previous = tx.upsert_balance(snapshot.clone()).await?;
        let changes = AuditChanges::new()
            .set("on", on)
            .change(
                "balance_minor",
                previous.as_ref().map(|p| p.balance_minor),
                minor_units,
            )
            .change(
                "source",
                previous.as_ref().map(|p| p.source.as_str()),
                BalanceSource::Manual.as_str(),
            );
        audit(&mut tx, now, actor, "balance.set", "account", id, changes).await?;
        tx.commit().await?;
        Ok(BalanceView::new(&snapshot, account.currency))
    }

    /// One page of an account's balances, newest day first; `from` is
    /// inclusive and `to` exclusive.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown or hidden account;
    /// [`LedgerError::Store`] with an invalid cursor; storage failures.
    pub async fn list_balances(
        &self,
        actor: &Actor,
        id: AccountId,
        from: Option<Date>,
        to: Option<Date>,
        page: &PageRequest,
    ) -> Result<BalancePage, LedgerError> {
        let access = self.access.resolve(actor).await?;
        authorize_account(actor, &access, id, AccountLevel::Balances)?;
        let account = self.store.get_account(id).await?;
        let page = self.store.list_balances(id, from, to, page).await?;
        Ok(BalancePage {
            items: page
                .items
                .iter()
                .map(|snapshot| BalanceView::new(snapshot, account.currency))
                .collect(),
            next: page.next,
        })
    }
}
