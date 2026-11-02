//! The household's settings (spec §6.8, §8.9, §8.10 N8).
//!
//! A safety floor is a figure of the plan over every account, so it takes the
//! forecast's access (R2) to read: below that it reads as `null`, the same as none
//! set, on a read and in a patch's answer alike. The other settings say how a forecast
//! is worked out and how numbers are shown, not what the household holds or plans.
//!
//! The display currency starts unset and is never guessed: a guess from the accounts
//! would tell a restricted token a hidden account's currency. For the same reason
//! changing it spans every account (N8): a visible row converts at once if the new
//! currency's rates are held, so a token could otherwise try a code and read from its own
//! report whether the household uses that currency.

use coffer_domain::audit::AuditChanges;
use coffer_domain::identity::Actor;
use coffer_domain::money::Currency;
use coffer_domain::plan::{ForecastSettings, PlanError, SafetyFloor};
use coffer_domain::policy::{
    AccountAccess, AccountLevel, Action, authorize, authorize_every_account,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Ledger, LedgerError, audit};

/// The audit entity of every settings change.
const ENTITY: &str = "household";

/// The settings to change. A field left out stays as it is. A floor can be
/// replaced but never unset: `null` leaves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ForecastSettingsPatch {
    /// A new safety floor; `null` or absent keeps the current one.
    #[serde(default)]
    pub floor: Option<SafetyFloor>,
    #[serde(default)]
    pub include_income: Option<bool>,
    /// 1 to 60 months.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 60)]
    pub default_horizon_months: Option<u8>,
    /// 1 to 365 days.
    #[serde(default)]
    #[schema(minimum = 1, maximum = 365)]
    pub stale_after_days: Option<u16>,
    /// A new display currency, an ISO 4217 code; `null` or absent keeps the current one.
    /// It can be replaced but never unset. Changing it recomputes all history in this
    /// request.
    #[serde(default)]
    pub display_currency: Option<Currency>,
}

/// The household's settings: those of a forecast and the display currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct HouseholdSettings {
    /// The safety floor. Until one is set a forecast has no number: a silent
    /// floor of 0 would overstate what can be invested. A token without the access a
    /// forecast needs reads `null` whether or not one is set.
    #[schema(required)]
    pub floor: Option<SafetyFloor>,
    /// Whether planned inflows reduce the cash needed.
    pub include_income: bool,
    /// The horizon of a forecast that names none: 1 to 60 months.
    #[schema(minimum = 1, maximum = 60)]
    pub default_horizon_months: u8,
    /// How old a balance may be before it is flagged: 1 to 365 days.
    #[schema(minimum = 1, maximum = 365)]
    pub stale_after_days: u16,
    /// What net worth is shown in; `null` until an admin sets it, and never guessed. It
    /// reads the same for every token: it says how numbers are shown, not what the
    /// household holds.
    #[schema(required)]
    pub display_currency: Option<Currency>,
}

impl HouseholdSettings {
    fn new(settings: ForecastSettings, display_currency: Option<Currency>) -> Self {
        let ForecastSettings {
            floor,
            include_income,
            default_horizon_months,
            stale_after_days,
        } = settings;
        Self {
            floor,
            include_income,
            default_horizon_months,
            stale_after_days,
            display_currency,
        }
    }
}

/// The message for a rejected setting; never echoes the input.
pub(super) fn invalid(error: PlanError) -> LedgerError {
    LedgerError::Invalid(match error {
        PlanError::InvalidFloor => "a fixed floor must be between 0 and 10^15 minor units",
        PlanError::InvalidFloorMonths => "floor months must be 1 to 60",
        PlanError::InvalidHorizon => "the horizon must be 1 to 60 months",
        PlanError::InvalidStaleDays => "stale after must be 1 to 365 days",
        // `PlanError` is non-exhaustive, and no other variant arises here.
        _ => "the forecast settings are not valid",
    })
}

/// `settings` as `actor` may have them: without the floor unless it may have a
/// forecast.
fn as_shown(actor: &Actor, access: &AccountAccess, settings: ForecastSettings) -> ForecastSettings {
    match authorize_every_account(actor, access, AccountLevel::Transactions) {
        Ok(()) => settings,
        Err(_) => ForecastSettings {
            floor: None,
            ..settings
        },
    }
}

impl Ledger {
    /// The household's settings: the defaults until some are set. The floor reads as
    /// none without the forecast's access.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ReadHousehold`]; a failure to resolve
    /// access; storage failures.
    pub async fn household_settings(
        &self,
        actor: &Actor,
    ) -> Result<HouseholdSettings, LedgerError> {
        authorize(actor, Action::ReadHousehold)?;
        let access = self.access.resolve(actor).await?;
        let (settings, display_currency) = self.store.household_settings().await?;
        Ok(HouseholdSettings::new(
            as_shown(actor, &access, settings),
            display_currency,
        ))
    }

    /// Changes the household's settings. A patch that changes nothing writes
    /// nothing, audit included, and only a new display currency rebuilds the daily
    /// values, in this transaction. The answer shows the floor as
    /// [`Self::household_settings`] does, so it can't confirm a guess at it.
    ///
    /// # Errors
    ///
    /// [`LedgerError::Denied`] without [`Action::ManageHousehold`], or for a patch naming a
    /// display currency, whatever it names, without `balances` access to every account;
    /// [`LedgerError::Invalid`] for a value out of range; a failure to resolve
    /// access; storage failures.
    pub async fn update_forecast_settings(
        &self,
        actor: &Actor,
        patch: ForecastSettingsPatch,
    ) -> Result<HouseholdSettings, LedgerError> {
        authorize(actor, Action::ManageHousehold)?;
        let access = self.access.resolve(actor).await?;
        // Decided from the request alone, before anything is read or checked.
        if patch.display_currency.is_some() {
            authorize_every_account(actor, &access, AccountLevel::Balances)?;
        }
        let now = self.clock.now();
        // `begin` takes `BEGIN IMMEDIATE`, so no writer can change the row
        // between this read and the no-op check below.
        let mut tx = self.store.begin().await?;
        let current = tx.forecast_settings().await?;
        let current_display = tx.display_currency().await?;
        let new_display = patch
            .display_currency
            .filter(|chosen| Some(*chosen) != current_display);
        let next = ForecastSettings {
            floor: patch.floor.or(current.floor),
            include_income: patch.include_income.unwrap_or(current.include_income),
            default_horizon_months: patch
                .default_horizon_months
                .unwrap_or(current.default_horizon_months),
            stale_after_days: patch.stale_after_days.unwrap_or(current.stale_after_days),
        };
        next.validate().map_err(invalid)?;

        let forecast_changed = next != current;
        let mut changes = AuditChanges::new();
        if next.floor != current.floor {
            changes = changes.change(
                "floor",
                current.floor.map(|floor| floor.canonical()),
                next.floor.map(|floor| floor.canonical()),
            );
        }
        if next.include_income != current.include_income {
            changes = changes.change(
                "include_income",
                current.include_income,
                next.include_income,
            );
        }
        if next.default_horizon_months != current.default_horizon_months {
            changes = changes.change(
                "default_horizon_months",
                i64::from(current.default_horizon_months),
                i64::from(next.default_horizon_months),
            );
        }
        if next.stale_after_days != current.stale_after_days {
            changes = changes.change(
                "stale_after_days",
                i64::from(current.stale_after_days),
                i64::from(next.stale_after_days),
            );
        }
        if let Some(chosen) = new_display {
            changes = changes.change(
                "display_currency",
                current_display.map(|known| known.as_str().to_owned()),
                chosen.as_str().to_owned(),
            );
        }
        if changes.is_empty() {
            return Ok(HouseholdSettings::new(
                as_shown(actor, &access, current),
                current_display,
            ));
        }
        if forecast_changed {
            tx.set_forecast_settings(&next).await?;
        }
        if let Some(chosen) = new_display {
            tx.set_display_currency(chosen).await?;
        }
        audit(
            &mut tx,
            now,
            actor,
            "household.settings",
            ENTITY,
            ENTITY,
            changes,
        )
        .await?;
        tx.commit().await?;
        Ok(HouseholdSettings::new(
            as_shown(actor, &access, next),
            new_display.or(current_display),
        ))
    }
}
