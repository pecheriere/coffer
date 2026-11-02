//! How the pages holding financial data write an amount and a day. The API's numbers
//! stay ungrouped: these only put separators into a page's text.

use coffer_domain::money::{Currency, Money, format_minor_units};
use jiff::civil::Date;

/// `minor` units of `currency` as a page writes them: `-1,234.56` USD, `1,234` JPY.
pub(super) fn grouped(minor: i64, currency: Currency) -> String {
    let plain = format_minor_units(minor, currency);
    let (sign, unsigned) = plain
        .strip_prefix('-')
        .map_or(("", plain.as_str()), |rest| ("-", rest));
    let (whole, decimals) = unsigned
        .split_once('.')
        .map_or((unsigned, None), |(whole, decimals)| {
            (whole, Some(decimals))
        });
    let mut text = String::with_capacity(plain.len() + whole.len() / 3);
    text.push_str(sign);
    for (index, digit) in whole.char_indices() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    if let Some(decimals) = decimals {
        text.push('.');
        text.push_str(decimals);
    }
    text
}

/// `amount` with its currency: `-1,234.56 USD`.
pub(super) fn money(amount: Money) -> String {
    format!(
        "{} {}",
        grouped(amount.minor_units, amount.currency),
        amount.currency.as_str()
    )
}

/// The size of `amount`, whatever its sign, with its currency: `12.34 USD`.
pub(super) fn magnitude(amount: Money) -> String {
    money(amount).trim_start_matches('-').to_owned()
}

pub(super) fn day(date: Date) -> String {
    date.strftime("%-d %B %Y").to_string()
}

#[cfg(test)]
mod tests {
    use coffer_domain::money::Currency;

    use super::grouped;

    #[test]
    fn amounts_are_grouped_in_threes_whatever_the_currency() {
        let group = |minor, code| grouped(minor, Currency::parse(code).unwrap());
        assert_eq!(group(0, "USD"), "0.00");
        assert_eq!(group(5, "USD"), "0.05");
        assert_eq!(group(99_999, "USD"), "999.99");
        assert_eq!(group(100_000, "USD"), "1,000.00");
        assert_eq!(group(-100_000, "USD"), "-1,000.00");
        assert_eq!(group(100_000_000, "USD"), "1,000,000.00");
        assert_eq!(group(i64::MIN, "USD"), "-92,233,720,368,547,758.08");
        assert_eq!(group(999, "JPY"), "999");
        assert_eq!(group(-1_000, "JPY"), "-1,000");
        assert_eq!(group(1_234_567, "KWD"), "1,234.567");
        assert_eq!(group(-1, "KWD"), "-0.001");
    }
}
