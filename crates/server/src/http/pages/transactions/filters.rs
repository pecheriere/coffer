//! The list's filters: read from its query string, strictly, and written back into one, so a
//! form returning to the list rebuilds its address from values already read, never from text
//! it was sent.

use coffer_domain::ids::{AccountId, CategoryId};
use coffer_domain::plan::strict_date;
use coffer_domain::transactions::{ReviewStatus, TransactionFlag};
use coffer_store::PageRequest;
use jiff::civil::Date;
use serde::Deserialize;

use super::LIST;
use crate::http::pages::PageError;
use crate::ledger::transactions::TransactionQueryInput;

/// The most characters of a search, as the API takes.
const MAX_SEARCH_CHARS: usize = 100;
/// Transactions on one page of the list.
pub(super) const PAGE_SIZE: u32 = 50;

/// Which categories to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum CategoryChoice {
    #[default]
    Any,
    Uncategorized,
    One(CategoryId),
}

/// The flags the list filters on.
pub(super) const FLAGS: [TransactionFlag; 3] = [
    TransactionFlag::PossibleDuplicate,
    TransactionFlag::SourceRemoved,
    TransactionFlag::SplitMismatch,
];

/// What the list shows. Every one given must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Filters {
    pub(super) account: Option<AccountId>,
    pub(super) from: Option<Date>,
    /// The last day shown, included.
    pub(super) until: Option<Date>,
    pub(super) review: Option<ReviewStatus>,
    pub(super) category: CategoryChoice,
    pub(super) flag: Option<TransactionFlag>,
    pub(super) text: Option<String>,
}

/// The list's query string. Every field is text, so an empty one, which a form sends for a
/// filter left alone, reads as no filter. Unknown fields are refused.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    account: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    until: String,
    #[serde(default)]
    review: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    flag: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    after: String,
}

/// A filter's text, or `None` when it was left empty.
fn given(text: &str) -> Option<&str> {
    (!text.is_empty()).then_some(text)
}

fn day(text: &str) -> Result<Option<Date>, PageError> {
    given(text)
        .map(|text| strict_date::parse(text).ok_or_else(|| PageError::invalid("not a real day")))
        .transpose()
}

impl Filters {
    /// The filters in `query`, and the cursor of the page asked for.
    pub(super) fn parse(query: Option<&str>) -> Result<(Self, Option<String>), PageError> {
        let query: ListQuery = serde_urlencoded::from_str(query.unwrap_or_default())
            .map_err(|_| PageError::invalid("the filters could not be read"))?;
        let account = given(&query.account)
            .map(|text| {
                text.parse()
                    .map_err(|_| PageError::invalid("not an account the list offers"))
            })
            .transpose()?;
        let review = given(&query.review)
            .map(|text| {
                text.parse()
                    .map_err(|_| PageError::invalid("not a review state the list offers"))
            })
            .transpose()?;
        let category = match given(&query.category) {
            None => CategoryChoice::Any,
            Some("none") => CategoryChoice::Uncategorized,
            Some(text) => CategoryChoice::One(
                text.parse()
                    .map_err(|_| PageError::invalid("not a category the list offers"))?,
            ),
        };
        let flag = given(&query.flag)
            .map(|text| {
                FLAGS
                    .into_iter()
                    .find(|flag| flag.as_str() == text)
                    .ok_or_else(|| PageError::invalid("not a mark the list offers"))
            })
            .transpose()?;
        let text = given(&query.q)
            .map(|text| {
                if text.chars().count() > MAX_SEARCH_CHARS {
                    Err(PageError::invalid("a search is at most 100 characters"))
                } else {
                    Ok(text.to_owned())
                }
            })
            .transpose()?;
        let filters = Self {
            account,
            from: day(&query.from)?,
            until: day(&query.until)?,
            review,
            category,
            flag,
            text,
        };
        Ok((filters, given(&query.after).map(str::to_owned)))
    }

    /// The filters as the list's query string, without a cursor; empty when there are none.
    pub(super) fn query(&self) -> String {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        if let Some(account) = self.account {
            pairs.push(("account", account.to_string()));
        }
        if let Some(from) = self.from {
            pairs.push(("from", from.to_string()));
        }
        if let Some(until) = self.until {
            pairs.push(("until", until.to_string()));
        }
        if let Some(review) = self.review {
            pairs.push(("review", review.as_str().to_owned()));
        }
        match self.category {
            CategoryChoice::Any => {}
            CategoryChoice::Uncategorized => pairs.push(("category", "none".to_owned())),
            CategoryChoice::One(id) => pairs.push(("category", id.to_string())),
        }
        if let Some(flag) = self.flag {
            pairs.push(("flag", flag.as_str().to_owned()));
        }
        if let Some(text) = &self.text {
            pairs.push(("q", text.clone()));
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.extend_pairs(pairs);
        query.finish()
    }

    /// The list with these filters, from its first page.
    pub(super) fn path(&self) -> String {
        let query = self.query();
        if query.is_empty() {
            LIST.to_owned()
        } else {
            format!("{LIST}?{query}")
        }
    }

    /// The list with these filters, from the page after `cursor`.
    pub(super) fn path_after(&self, cursor: &str) -> String {
        let after = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("after", cursor)
            .finish();
        let query = self.query();
        if query.is_empty() {
            format!("{LIST}?{after}")
        } else {
            format!("{LIST}?{query}&{after}")
        }
    }

    /// What the service is asked for. "Until" includes its day; the API's `to` doesn't.
    pub(super) fn input(&self, after: Option<String>) -> Result<TransactionQueryInput, PageError> {
        let to = self
            .until
            .map(|until| {
                until
                    .tomorrow()
                    .map_err(|_| PageError::invalid("not a real day"))
            })
            .transpose()?;
        let (categories, uncategorized) = match self.category {
            CategoryChoice::Any => (Vec::new(), false),
            CategoryChoice::Uncategorized => (Vec::new(), true),
            CategoryChoice::One(id) => (vec![id], false),
        };
        Ok(TransactionQueryInput {
            accounts: self.account.into_iter().collect(),
            from: self.from,
            to,
            review: self.review,
            assignee: None,
            categories,
            uncategorized,
            unmatched_transfer: false,
            rule: None,
            pending: None,
            flag: self.flag,
            min_minor: None,
            max_minor: None,
            text: self.text.clone(),
            page: PageRequest {
                after,
                limit: PAGE_SIZE,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CategoryChoice, Filters};

    #[test]
    fn filters_read_back_from_the_query_they_write() {
        let query = "account=0190f5a0-0000-7000-8000-000000000001&from=2026-10-01\
                     &until=2026-10-31&review=unreviewed&category=none\
                     &flag=possible_duplicate&q=caf%C3%A9+%26+co";
        let (filters, after) = Filters::parse(Some(query)).unwrap();
        assert_eq!(after, None);
        assert_eq!(filters.category, CategoryChoice::Uncategorized);
        assert_eq!(filters.text.as_deref(), Some("café & co"));
        let (again, _) = Filters::parse(Some(&filters.query())).unwrap();
        assert_eq!(again, filters);
    }

    #[test]
    fn empty_fields_are_no_filter_and_unknown_ones_are_refused() {
        let (filters, after) = Filters::parse(Some(
            "account=&from=&until=&review=&category=&flag=&q=&after=",
        ))
        .unwrap();
        assert_eq!(filters.path(), "/transactions");
        assert_eq!(after, None);
        assert!(Filters::parse(Some("sort=amount")).is_err());
        assert!(Filters::parse(Some("from=2026-02-30")).is_err());
        assert!(Filters::parse(Some("review=maybe")).is_err());
        assert!(Filters::parse(Some(&format!("q={}", "x".repeat(101)))).is_err());
    }

    #[test]
    fn the_next_page_keeps_the_filters() {
        let (filters, _) = Filters::parse(Some("review=unreviewed")).unwrap();
        assert_eq!(
            filters.path_after("abc+/="),
            "/transactions?review=unreviewed&after=abc%2B%2F%3D"
        );
    }
}
