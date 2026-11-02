//! Exchange rates from outside coffer (spec §6.10, §8.10 N6): the interface a
//! rate source implements, and the one place sources are registered.
//!
//! A source's answer is untrusted. Nothing of it reaches an error, a log line
//! or the store except rates and dates read strictly, and a source that fails
//! leaves rates missing, never wrong.

pub mod ecb;

use std::sync::Arc;

use coffer_domain::fx::{FxErrorCode, FxSource, RateObservation};
use coffer_domain::money::Currency;
use jiff::civil::Date;
use thiserror::Error;

use self::ecb::{ECB_HOST, Ecb};
use crate::config::{Config, ConfigError, FxProviderKind};
use crate::egress::{Egress, HostAllowList};
use crate::sync::provider::ProviderFuture;

/// A source of exchange rates.
pub trait RateProvider: Send + Sync {
    /// Who publishes these rates.
    fn source(&self) -> FxSource;

    /// Units of `currency` per euro, oldest first, from `since` when given.
    /// Reads no clock: `today` bounds what it accepts.
    fn fetch(
        &self,
        currency: Currency,
        since: Option<Date>,
        today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>>;
}

/// Why a source gave no rates. Messages are fixed and no variant holds text
/// from the source: at most its status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RateError {
    /// The source has no such series.
    #[error("the rate source does not publish this currency")]
    NotPublished,
    /// The source answered with an error, or with a redirect, which is not
    /// followed.
    #[error("the rate source answered with status {status}")]
    Unavailable {
        /// The response's status.
        status: u16,
    },
    /// The source's answer could not be read, or was too large.
    #[error("the rate source's answer could not be read")]
    Malformed,
    /// The source could not be reached.
    #[error("the rate source could not be reached")]
    Network,
}

impl RateError {
    /// The code to record for this failure; `None` for
    /// [`RateError::NotPublished`], which is an answer and not a failure.
    #[must_use]
    pub fn code(self) -> Option<FxErrorCode> {
        match self {
            Self::NotPublished => None,
            Self::Unavailable { .. } => Some(FxErrorCode::Unavailable),
            Self::Malformed => Some(FxErrorCode::Malformed),
            Self::Network => Some(FxErrorCode::Network),
        }
    }
}

/// The rate source the configuration names, if any: the one place rate
/// providers are registered.
///
/// # Errors
///
/// [`ConfigError`] when a source's host cannot form an allow-list, which a
/// known source's can.
pub fn rate_provider(
    config: &Config,
    egress: &Egress,
) -> Result<Option<Arc<dyn RateProvider>>, ConfigError> {
    match config.fx_provider {
        FxProviderKind::Off => Ok(None),
        FxProviderKind::Ecb => {
            let allowed = HostAllowList::parse(ECB_HOST).map_err(|_| ConfigError::Invalid {
                var: "COFFER_FX_PROVIDER",
                reason: "its host could not be used",
            })?;
            Ok(Some(Arc::new(Ecb::new(egress.clone(), allowed))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Egress, FxErrorCode, FxSource, RateError, rate_provider};

    fn config(provider: Option<&str>) -> Config {
        Config::from_env(|key| match key {
            "COFFER_PUBLIC_URL" => Some("https://coffer.example".to_owned()),
            "COFFER_DATA_DIR" => Some("/data".to_owned()),
            "COFFER_FX_PROVIDER" => provider.map(str::to_owned),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn fx_provider_ecb_registers_the_ecb() {
        let egress = Egress::new(None).unwrap();
        let provider = rate_provider(&config(None), &egress).unwrap();
        assert_eq!(provider.map(|found| found.source()), Some(FxSource::Ecb));
    }

    #[test]
    fn fx_provider_none_registers_nothing() {
        let egress = Egress::new(None).unwrap();
        let provider = rate_provider(&config(Some("none")), &egress).unwrap();
        assert!(provider.is_none(), "no source may be registered");
    }

    #[test]
    fn a_series_that_is_not_published_is_not_a_failure() {
        assert_eq!(RateError::NotPublished.code(), None);
    }

    #[test]
    fn each_failure_has_its_own_code() {
        assert_eq!(
            RateError::Unavailable { status: 503 }.code(),
            Some(FxErrorCode::Unavailable)
        );
        assert_eq!(RateError::Malformed.code(), Some(FxErrorCode::Malformed));
        assert_eq!(RateError::Network.code(), Some(FxErrorCode::Network));
    }
}
