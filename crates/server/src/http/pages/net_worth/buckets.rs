//! What isn't part of net worth (spec §8.11 B6): each bucket as the report answers it, with its
//! own status, reasons, accounts and periods. Nothing here adds one to another or to the
//! household's, and no bucket has a chart: one line on the page stays net worth, and one per
//! bucket would invite adding them by eye.

use coffer_domain::net_worth::{NetWorthBucket, NetWorthPoint, NetWorthReport, NetWorthStatus};
use maud::{Markup, html};

use super::words::{
    Names, bucket_blocker_sentence, bucket_caution_sentence, bucket_heading, manual_rate_sentence,
};
use super::{
    NO_TOTAL, NO_VALUE_THAT_DAY, accounts_table, by_currency_table, left_out_notes,
    opens_for_a_gap, periods, periods_table, status,
};
use crate::http::pages::amounts::{day, money};

/// A bucket's account is never counted in net worth, so the table says what it is left out of.
const NOT_IN_THIS_TOTAL: &str = "Not in this total";

/// Every bucket in the report's order, or nothing with none.
pub(super) fn section(report: &NetWorthReport) -> Markup {
    if report.buckets.is_empty() {
        return html! {};
    }
    html! {
        section.card id="held" {
            h2 { "Not part of your net worth" }
            p {
                "Held for someone else or given away. None of this is in the number above, and \
                 it isn't added to it."
            }
            @for bucket in &report.buckets {
                (one_bucket(report, bucket))
            }
        }
    }
}

fn one_bucket(report: &NetWorthReport, bucket: &NetWorthBucket) -> Markup {
    let heading = bucket_heading(bucket.belongs_to, bucket.beneficiary.as_deref());
    let latest = bucket.points.last();
    let names = Names::of_accounts(&bucket.accounts, latest.map(|point| point.on));
    let has_a_total = latest.is_some_and(|point| point.total.is_some());
    let accounts_label = format!("{heading}: accounts at the latest point");
    let sums = sums_with_no_total(latest);
    // A day shows above to say "that day" of; with no point there is none.
    let no_value = if latest.is_some() {
        NO_VALUE_THAT_DAY
    } else {
        "No value in this range"
    };
    html! {
        h3 { (heading) }
        (latest_total(latest))
        @if bucket.status != NetWorthStatus::Firm {
            (status(bucket.status, has_a_total))
        }
        @for used in &bucket.manual_rates {
            p.fine { (manual_rate_sentence(*used)) }
        }
        @if !bucket.blockers.is_empty() || !bucket.cautions.is_empty() {
            ul.reasons.held-reasons {
                @for blocker in &bucket.blockers {
                    li { (bucket_blocker_sentence(*blocker, sums.is_some())) }
                }
                @for caution in &bucket.cautions {
                    li { (bucket_caution_sentence(caution, &names, has_a_total)) }
                }
            }
        }
        @if let Some(point) = sums {
            p.fine { "By currency, unconverted" }
            (by_currency_table(&point.by_currency))
            (left_out_notes(&bucket.accounts, point, &names))
        }
        (accounts_table(
            &bucket.accounts,
            report.display_currency,
            latest.map(|point| point.on),
            &accounts_label,
            no_value,
            NOT_IN_THIS_TOTAL,
        ))
        (every_value(report, bucket, &names, &heading))
    }
}

/// The latest point's total and its day. An earlier total is not offered instead: it would
/// read as the value now.
fn latest_total(latest: Option<&NetWorthPoint>) -> Markup {
    html! {
        @match latest {
            Some(NetWorthPoint { on, total: Some(total), .. }) => {
                p { strong.hero-number { (money(*total)) } " on " (day(*on)) }
            }
            Some(NetWorthPoint { on, .. }) => {
                p { strong { (NO_TOTAL) " on " (day(*on)) } }
            }
            None => {
                p { strong { (NO_TOTAL) } }
            }
        }
    }
}

/// With no total, a point with sums can still say them: within this bucket, per currency, as
/// the display currency's blocker promises.
fn sums_with_no_total(latest: Option<&NetWorthPoint>) -> Option<&NetWorthPoint> {
    latest.filter(|point| point.total.is_none() && !point.by_currency.is_empty())
}

/// The bucket's periods, as the household's table lists its own. With no display currency
/// there are no totals to list.
fn every_value(
    report: &NetWorthReport,
    bucket: &NetWorthBucket,
    names: &Names<'_>,
    heading: &str,
) -> Markup {
    let Some(currency) = report.display_currency else {
        return html! {};
    };
    let periods = periods(&bucket.points, report.from, report.interval);
    if periods.is_empty() {
        return html! {};
    }
    let label = format!("{heading}: every period");
    html! {
        details.values open[opens_for_a_gap(&periods)] {
            summary { "Every value, as a table" }
            (periods_table(
                &bucket.accounts,
                report.interval,
                &periods,
                currency.as_str(),
                names,
                &label,
                "Nothing in this total",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use coffer_domain::accounts::LiquidityClass;
    use coffer_domain::ids::AccountId;
    use coffer_domain::money::Currency;
    use coffer_domain::net_worth::{AccountSeries, NotCountedReason};

    use super::super::account_cells;
    use super::{NO_VALUE_THAT_DAY, NOT_IN_THIS_TOTAL};

    /// The report never leaves a bucket's account out of its sums, so only a series made here can
    /// reach this, and its type doesn't rule it out.
    #[test]
    fn a_series_left_out_says_the_words_it_is_given() {
        let series = AccountSeries {
            account: AccountId::new(),
            name: "Fund".to_owned(),
            liquidity: LiquidityClass::Invested,
            currency: Currency::parse("USD").unwrap(),
            counted: false,
            not_counted: Some(NotCountedReason::ExcludedByFlag),
            first_on: None,
            closed_on: None,
            points: Vec::new(),
        };

        let cells = account_cells(&series, None, NO_VALUE_THAT_DAY, NOT_IN_THIS_TOTAL);

        assert_eq!(cells[0].value, NOT_IN_THIS_TOTAL);
        assert_eq!(cells[1].value, NOT_IN_THIS_TOTAL);
    }
}
