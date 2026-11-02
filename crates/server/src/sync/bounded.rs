//! A list read up to a limit, shared by the providers' parsers.

use std::fmt;

use serde::Deserializer;
use serde::de::{IgnoredAny, SeqAccess, Visitor};
use serde_json::value::RawValue;

/// The entries of `list`, up to one past `max`; the rest are read past without
/// being held, and must still be valid JSON. A caller that finds more than
/// `max` entries refuses the whole answer before converting any of them.
pub(crate) fn bounded<'de, D: Deserializer<'de>>(
    list: D,
    max: usize,
) -> Result<Vec<&'de RawValue>, D::Error> {
    struct Limited(usize);

    impl<'de> Visitor<'de> for Limited {
        type Value = Vec<&'de RawValue>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of records")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut list: A) -> Result<Self::Value, A::Error> {
            let mut kept = Vec::new();
            while kept.len() <= self.0 {
                match list.next_element()? {
                    Some(entry) => kept.push(entry),
                    None => return Ok(kept),
                }
            }
            while list.next_element::<IgnoredAny>()?.is_some() {}
            Ok(kept)
        }
    }

    list.deserialize_seq(Limited(max))
}
