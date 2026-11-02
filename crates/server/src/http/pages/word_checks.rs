//! What the tests of a page's words check: the kinds the API's schema lists, and that a
//! sentence reads as one.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use utoipa::PartialSchema;

/// Every value of `kind` in `schema`, walking its alternatives.
fn kinds_in(schema: &Value, found: &mut BTreeSet<String>) {
    match schema {
        Value::Object(fields) => {
            if let Some(kinds) = fields
                .get("properties")
                .and_then(|properties| properties["kind"]["enum"].as_array())
            {
                found.extend(kinds.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            fields.values().for_each(|value| kinds_in(value, found));
        }
        Value::Array(values) => values.iter().for_each(|value| kinds_in(value, found)),
        _ => {}
    }
}

/// What the API's schema says the kinds of `T` are.
pub(super) fn schema_kinds<T: PartialSchema>() -> BTreeSet<String> {
    let schema = serde_json::to_value(T::schema()).unwrap();
    let mut found = BTreeSet::new();
    kinds_in(&schema, &mut found);
    assert!(!found.is_empty(), "the schema must list the kinds");
    found
}

pub(super) fn sampled_kinds<T: Serialize>(samples: &[T]) -> BTreeSet<String> {
    samples
        .iter()
        .map(|sample| {
            serde_json::to_value(sample).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// A sentence a person could read: not empty, and not the API's wire name.
pub(super) fn assert_worded(sentence: &str, kinds: &BTreeSet<String>) {
    assert!(sentence.ends_with('.'), "a sentence ends");
    assert!(!sentence.contains('_'), "no wire name");
    for kind in kinds {
        assert!(!sentence.contains(kind.as_str()), "no wire name");
    }
}
