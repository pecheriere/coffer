//! The European Central Bank's reference rates, from its data API (spec §8.10
//! N6): one CSV series per currency, no key.
//!
//! The answer is read strictly and whole: one row that isn't exactly what is
//! expected refuses all of it, so rates are missing rather than wrong, and
//! nothing of it reaches an error or a log.

use coffer_domain::fx::{FxRate, FxSource, RateObservation};
use coffer_domain::money::Currency;
use jiff::civil::{Date, date};
use reqwest::{Method, StatusCode};
use url::Url;

use super::{RateError, RateProvider};
use crate::egress::{Egress, EgressError, HostAllowList, OutboundRequest};
use crate::sync::provider::ProviderFuture;

/// The one host this source reaches.
pub const ECB_HOST: &str = "data-api.ecb.europa.eu";

/// The largest answer read: USD's whole history was 408,646 bytes on
/// 2026-11-02, and the largest of the ECB's 39 series 421,335.
const MAX_RATE_BODY: usize = 2 * 1024 * 1024;
/// The most rows read, counting those with no value. The longest series held
/// 7,188 on 2026-11-02.
const MAX_RATE_ROWS: usize = 20_000;
const HEADER: &str = "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE";
/// No row is older: a bad one can't invent history before the euro's rates.
const EARLIEST: Date = date(1990, 1, 1);

/// The European Central Bank as a [`RateProvider`].
#[derive(Debug, Clone)]
pub struct Ecb {
    egress: Egress,
    allowed: HostAllowList,
    /// Scheme and authority, with no trailing slash.
    origin: String,
}

impl Ecb {
    /// The source at [`ECB_HOST`], sending through `egress` to `allowed`.
    #[must_use]
    pub fn new(egress: Egress, allowed: HostAllowList) -> Self {
        Self {
            egress,
            allowed,
            origin: format!("https://{ECB_HOST}"),
        }
    }

    /// The source at `base`, a loopback mock.
    #[cfg(feature = "testkit")]
    #[must_use]
    pub fn with_base(egress: Egress, allowed: HostAllowList, base: Url) -> Self {
        Self {
            egress,
            allowed,
            origin: String::from(base).trim_end_matches('/').to_owned(),
        }
    }

    /// The series' URL. A currency is a code from the built-in table and a
    /// date prints as digits and dashes, so neither needs escaping.
    fn series_url(&self, currency: Currency, since: Option<Date>) -> Result<Url, RateError> {
        let origin = &self.origin;
        let window = since.map_or(String::new(), |since| format!("&startPeriod={since}"));
        Url::parse(&format!(
            "{origin}/service/data/EXR/D.{currency}.EUR.SP00.A?format=csvdata&detail=dataonly{window}"
        ))
        .map_err(|_| RateError::Network)
    }

    async fn fetch_series(
        &self,
        currency: Currency,
        since: Option<Date>,
        today: Date,
    ) -> Result<Vec<RateObservation>, RateError> {
        // The euro is 1 on every day, and the ECB has no such series.
        if currency.as_str() == "EUR" {
            return Ok(Vec::new());
        }
        let request = OutboundRequest {
            method: Method::GET,
            url: self.series_url(currency, since)?,
            basic_auth: None,
            headers: Vec::new(),
            body: None,
        };
        let sent = self
            .egress
            .send_limited(request, &self.allowed, MAX_RATE_BODY)
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(EgressError::TooLarge { status }) if status == StatusCode::OK => {
                return Err(RateError::Malformed);
            }
            Err(EgressError::TooLarge { status }) => return Err(refusal(status)),
            Err(_) => return Err(RateError::Network),
        };
        if response.status != StatusCode::OK {
            return Err(refusal(response.status));
        }
        parse_series(&response.body, currency, since, today)
    }
}

impl RateProvider for Ecb {
    fn source(&self) -> FxSource {
        FxSource::Ecb
    }

    fn fetch(
        &self,
        currency: Currency,
        since: Option<Date>,
        today: Date,
    ) -> ProviderFuture<'_, Result<Vec<RateObservation>, RateError>> {
        Box::pin(self.fetch_series(currency, since, today))
    }
}

/// What a status other than 200 means. Its body is never looked at.
fn refusal(status: StatusCode) -> RateError {
    if status == StatusCode::NOT_FOUND {
        RateError::NotPublished
    } else {
        RateError::Unavailable {
            status: status.as_u16(),
        }
    }
}

/// Reads a 200's body. An empty one is a window with no rates. A row with an
/// empty value is a day with no rate, as the ECB's series have on some holidays
/// up to 2012: it says less than leaving the row out would, so it passes every
/// other check and yields nothing.
fn parse_series(
    body: &[u8],
    currency: Currency,
    since: Option<Date>,
    today: Date,
) -> Result<Vec<RateObservation>, RateError> {
    let text = std::str::from_utf8(body).map_err(|_| RateError::Malformed)?;
    // `lines` takes off one `\n` or `\r\n`, and nothing else.
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Ok(Vec::new());
    };
    if header != HEADER {
        return Err(RateError::Malformed);
    }
    // The ECB's day can be ahead of the household's, by a day at most.
    let latest = today.tomorrow().map_err(|_| RateError::Malformed)?;
    let earliest = since.map_or(EARLIEST, |since| since.max(EARLIEST));
    let fields = format!("EXR.D.{currency}.EUR.SP00.A,D,{currency},EUR,SP00,A,");

    let mut rates = Vec::new();
    let mut previous = None;
    for (index, line) in lines.enumerate() {
        if index >= MAX_RATE_ROWS {
            return Err(RateError::Malformed);
        }
        // Whatever follows the date is the value, so a ninth field fails as
        // a rate.
        let (on, value) = line
            .strip_prefix(&fields)
            .and_then(|rest| rest.split_once(','))
            .ok_or(RateError::Malformed)?;
        let on = strict_date(on).ok_or(RateError::Malformed)?;
        if on < earliest || on > latest || previous.is_some_and(|before| on <= before) {
            return Err(RateError::Malformed);
        }
        previous = Some(on);
        if !value.is_empty() {
            let rate: FxRate = value.parse().map_err(|_| RateError::Malformed)?;
            rates.push(RateObservation { on, rate });
        }
    }
    Ok(rates)
}

/// `YYYY-MM-DD` and a real day, nothing else: `jiff` alone would also take a
/// date with a time, or one with no dashes.
fn strict_date(text: &str) -> Option<Date> {
    let shape_ok = text.len() == 10
        && text.bytes().enumerate().all(|(at, byte)| match at {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if shape_ok { text.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use super::{ECB_HOST, Egress, HostAllowList, Url};

    #[test]
    fn the_host_is_the_documented_one() {
        let configuration = include_str!("../../../../docs/configuration.md");
        assert!(
            configuration.contains(ECB_HOST),
            "the configuration guide must name the host"
        );
        let spec = include_str!("../../../../docs/superpowers/specs/2026-10-30-coffer-design.md");
        let (_, section) = spec.split_once("### 8.10 ").unwrap();
        let section = section.split("\n## ").next().unwrap();
        let section = section.split("\n### ").next().unwrap();
        assert!(section.contains(ECB_HOST), "spec 8.10 must name the host");
    }

    #[test]
    fn the_allow_list_refuses_every_other_host() {
        let egress = Egress::new(None).unwrap();
        let allowed = HostAllowList::parse(ECB_HOST).unwrap();
        let check = |host: &str| {
            let url = Url::parse(&format!("https://{host}/service/data/EXR/x")).unwrap();
            egress.check_url(&url, &allowed).is_ok()
        };
        assert!(check(ECB_HOST), "the host itself must be allowed");
        assert!(!check("www.ecb.europa.eu"), "a sibling must be refused");
        assert!(
            !check("data-api.ecb.europa.eu.evil.example"),
            "a longer name must be refused"
        );
    }
}
