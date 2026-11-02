//! Whose money an account is, and what follows from that: one rule for an account being
//! created, bound or changed (spec §8.11 B2, B4, B5).

use super::{AccountError, AccountKind, BelongsTo, Beneficiary, default_belongs_to};

/// What an account says about whose money it is, and what follows from that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Belonging {
    pub belongs_to: BelongsTo,
    /// Whether `belongs_to` was chosen rather than defaulted from the kind. A defaulted one
    /// follows a change of kind; a chosen one stays.
    pub overridden: bool,
    pub beneficiary: Option<Beneficiary>,
    pub include_in_net_worth: bool,
    pub include_in_forecast: bool,
}

/// What a request asks of an account's beneficiary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeneficiaryChange {
    Keep,
    Clear,
    Set(Beneficiary),
}

impl BeneficiaryChange {
    /// What the `beneficiary` of a request asks: absent keeps it, since a field left out
    /// changes nothing, so empty after trimming removes it.
    ///
    /// # Errors
    ///
    /// [`AccountError::InvalidBeneficiary`] for text that can't be a beneficiary.
    pub fn parse(asked: Option<&str>) -> Result<Self, AccountError> {
        match asked.map(str::trim) {
            None => Ok(Self::Keep),
            Some("") => Ok(Self::Clear),
            Some(label) => Beneficiary::parse(label).map(Self::Set),
        }
    }
}

/// What a request asks about whose money an account is. `None` asks nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BelongingAsked {
    pub belongs_to: Option<BelongsTo>,
    pub beneficiary: BeneficiaryChange,
    pub include_in_net_worth: Option<bool>,
    pub include_in_forecast: Option<bool>,
}

/// The one rule, for an account being created (`current` is `None`) or changed (spec §8.11 B4).
///
/// # Errors
///
/// [`AccountError::CountedButNotTheHouseholds`] for a flag asked on for money that isn't the
/// household's; [`AccountError::BeneficiaryOnTheHousehold`] for a label on money that is.
pub fn resolve_belonging(
    current: Option<&Belonging>,
    kind: AccountKind,
    asked: &BelongingAsked,
) -> Result<Belonging, AccountError> {
    let chosen = current
        .filter(|account| account.overridden)
        .map(|account| account.belongs_to);
    let belongs_to = asked
        .belongs_to
        .or(chosen)
        .unwrap_or_else(|| default_belongs_to(kind));
    let overridden = asked.belongs_to.is_some() || chosen.is_some();

    if belongs_to != BelongsTo::Household {
        if asked.include_in_net_worth == Some(true) || asked.include_in_forecast == Some(true) {
            return Err(AccountError::CountedButNotTheHouseholds);
        }
        let beneficiary = match &asked.beneficiary {
            BeneficiaryChange::Keep => current.and_then(|account| account.beneficiary.clone()),
            BeneficiaryChange::Clear => None,
            BeneficiaryChange::Set(label) => Some(label.clone()),
        };
        return Ok(Belonging {
            belongs_to,
            overridden,
            beneficiary,
            include_in_net_worth: false,
            include_in_forecast: false,
        });
    }

    if matches!(asked.beneficiary, BeneficiaryChange::Set(_)) {
        return Err(AccountError::BeneficiaryOnTheHousehold);
    }
    let was_counted = current.filter(|account| account.belongs_to == BelongsTo::Household);
    Ok(Belonging {
        belongs_to,
        overridden,
        beneficiary: None,
        include_in_net_worth: asked
            .include_in_net_worth
            .or(was_counted.map(|account| account.include_in_net_worth))
            .unwrap_or(true),
        include_in_forecast: asked
            .include_in_forecast
            .or(was_counted.map(|account| account.include_in_forecast))
            .unwrap_or(true),
    })
}

#[cfg(test)]
mod tests;
