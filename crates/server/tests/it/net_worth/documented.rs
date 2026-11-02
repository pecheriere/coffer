//! What the API description has to say of net worth: a reader of the document alone can
//! tell what each part of the answer means.

use coffer_server::http::api::openapi::document;
use serde_json::Value;

fn schema(name: &str) -> Value {
    let doc = serde_json::to_value(document()).unwrap();
    doc["components"]["schemas"][name].clone()
}

fn described(schema: &Value, property: &str) -> String {
    schema["properties"][property]["description"]
        .as_str()
        .unwrap_or_else(|| panic!("a property has no description"))
        .to_owned()
}

#[test]
fn a_floor_is_always_answered() {
    let required = schema("HouseholdSettings")["required"].clone();

    assert!(
        required
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "floor"),
        "floor is required"
    );
}

#[test]
fn the_report_says_what_each_field_means() {
    let report = schema("NetWorthReport");
    for property in ["from", "to", "status", "blockers", "cautions"] {
        assert_ne!(described(&report, property), "");
    }
    assert_ne!(described(&schema("NetWorthPoint"), "period_start"), "");
}

#[test]
fn a_missing_rate_says_what_its_dates_are() {
    let caution = schema("NetWorthCaution");
    let variants = caution["oneOf"].as_array().unwrap();
    let missing = variants
        .iter()
        .find(|variant| variant["properties"]["kind"]["enum"][0] == "rate_missing")
        .unwrap();

    for property in ["first_on", "last_on"] {
        assert_ne!(described(missing, property), "");
    }
}

#[test]
fn each_blocker_is_explained() {
    let blocker = schema("NetWorthBlocker");
    let text = blocker["description"].as_str().unwrap();

    for kind in [
        "display_currency_not_set",
        "rebuilding",
        "no_counted_accounts",
    ] {
        assert!(
            text.contains(&format!("`{kind}`:")),
            "a kind is not explained"
        );
    }
}

#[test]
fn an_excluded_series_says_it_has_no_points() {
    let text = described(&schema("AccountSeries"), "points");

    assert!(text.contains("switched out"), "the exclusion is not said");
}

/// A reader of the document alone must not take a bucket for part of net worth.
#[test]
fn the_description_says_nothing_adds_a_bucket() {
    let doc = serde_json::to_value(document()).unwrap();
    let text = doc["paths"]["/api/v1/reports/net-worth"]["get"]["description"]
        .as_str()
        .unwrap()
        .to_owned();

    assert!(
        text.contains("nothing adds a bucket to `total`, or one bucket to another"),
        "the rule is not said"
    );
}

#[test]
fn a_bucket_always_says_its_label() {
    let required = schema("NetWorthBucket")["required"].clone();

    assert!(
        required
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "beneficiary"),
        "beneficiary is required"
    );
}

/// The refusal of a `from` after today is in the parameter's own text.
#[test]
fn a_from_after_today_is_documented() {
    let doc = serde_json::to_value(document()).unwrap();
    let parameters = doc["paths"]["/api/v1/reports/net-worth"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .clone();
    let from = parameters.iter().find(|one| one["name"] == "from").unwrap();

    assert!(
        from["description"]
            .as_str()
            .unwrap()
            .contains("after today"),
        "the refusal is not said"
    );
}
