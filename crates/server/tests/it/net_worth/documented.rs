//! What the API description has to say of net worth: a reader of the document alone can
//! tell what each part of the answer means.

use coffer_server::http::api::openapi::document;
use serde_json::{Value, json};

use super::{World, world};

/// The guide an agent reads for this report.
const GUIDE: &str = include_str!("../../../../../docs/agent-net-worth.md");

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
fn emergency_funds_are_always_answered() {
    let required = schema("HouseholdSettings")["required"].clone();

    assert!(
        required
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "emergency_funds"),
        "emergency_funds is required"
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

/// Every `kind` the schema `name` answers.
fn kinds_of(name: &str) -> Vec<String> {
    schema(name)["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| {
            variant["properties"]["kind"]["enum"][0]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The text from the heading `### title` up to the next heading of any level.
fn guide_section(title: &str) -> &'static str {
    let heading = format!("### {title}\n");
    let (_, rest) = GUIDE.split_once(heading.as_str()).unwrap();
    rest.split("\n#").next().unwrap()
}

/// The guide's table has a row for each thing the report can raise.
#[test]
fn the_guide_has_a_row_for_each_blocker_and_caution() {
    let table = guide_section("Status, blockers and cautions");
    for kind in kinds_of("NetWorthBlocker")
        .into_iter()
        .chain(kinds_of("NetWorthCaution"))
    {
        assert!(
            table.contains(&format!("\n| `{kind}`")),
            "{kind} has no row"
        );
    }
}

/// The same words mean something else of money that isn't in the total, so the guide says what
/// each comes to there.
#[test]
fn the_guide_reads_each_blocker_and_caution_for_a_bucket() {
    let section = guide_section("Blockers and cautions in a bucket");
    for kind in kinds_of("NetWorthBlocker")
        .into_iter()
        .chain(kinds_of("NetWorthCaution"))
    {
        assert!(section.contains(&format!("`{kind}`")), "{kind} is not read");
    }
}

/// The guide's rules on adding a bucket, in words that can't drift unnoticed.
#[test]
fn the_guide_says_nothing_adds_a_bucket() {
    let section = guide_section("Buckets: money that isn't the household's");
    let section = section.split_whitespace().collect::<Vec<_>>().join(" ");
    for rule in [
        "**Never add a bucket to `total`, or one bucket to another, and never call a sum of \
         them net worth.**",
        "`accounts` is the household's alone",
    ] {
        assert!(section.contains(rule), "a rule is not said");
    }
}

/// The element of `listed` that `documented` stands for: an account by its name and a point by
/// its day, since the guide shows only some of them, and otherwise the one in its place.
fn counterpart<'a>(documented: &Value, place: usize, listed: &'a [Value]) -> &'a Value {
    let found = ["name", "on"]
        .into_iter()
        .find(|field| documented.get(*field).is_some())
        .map_or(listed.get(place), |field| {
            listed.iter().find(|one| one[field] == documented[field])
        });
    found.unwrap_or_else(|| panic!("the guide shows what the report doesn't: {documented}"))
}

/// Whether the guide's `documented` is what the report answered. `"..."` in a list leaves that
/// list out and `<name>` stands for an id; every other key and value must be there and the same.
fn assert_agrees(documented: &Value, answered: &Value, at: &str) {
    match (documented, answered) {
        (Value::Object(keys), Value::Object(actual)) => {
            let shown: Vec<&String> = keys.keys().collect();
            let real: Vec<&String> = actual.keys().collect();
            assert_eq!(shown, real, "the keys at {at}");
            for (key, value) in keys {
                assert_agrees(value, &actual[key], &format!("{at}.{key}"));
            }
        }
        (Value::Array(items), Value::Array(actual)) => {
            if items.as_slice() == [json!("...")] {
                return;
            }
            let by_identity = items
                .iter()
                .any(|one| one.get("name").is_some() || one.get("on").is_some());
            if !by_identity {
                assert_eq!(items.len(), actual.len(), "the length at {at}");
            }
            for (place, item) in items.iter().enumerate() {
                assert_agrees(
                    item,
                    counterpart(item, place, actual),
                    &format!("{at}[{place}]"),
                );
            }
        }
        (Value::String(text), Value::String(_)) if text.starts_with('<') => {}
        _ => assert_eq!(documented, answered, "the value at {at}"),
    }
}

impl World {
    /// The household of the guide's first example.
    async fn the_guide_s_household(&self) {
        self.rates(
            "USD",
            &[
                ("2026-10-26", "1.1188"),
                ("2026-10-27", "1.115"),
                ("2026-10-28", "1.1133"),
                ("2026-10-29", "1.113"),
                ("2026-10-30", "1.1172"),
            ],
        )
        .await;
        let checking = self.holding("Checking", "USD").await;
        self.balance(checking, "2026-10-29", 500_000).await;
        let card = self
            .holding_with("Card", "credit_card", "USD", json!({}))
            .await;
        self.balance(card, "2026-10-29", -30_000).await;
        let savings = self
            .holding_with("Savings", "savings", "EUR", json!({}))
            .await;
        self.balance(savings, "2026-10-29", 100_000).await;
        let college = self
            .holding_with(
                "College fund",
                "education",
                "USD",
                json!({"beneficiary": "Child A"}),
            )
            .await;
        self.balance(college, "2026-10-26", 2_000_000).await;
        let utma = self
            .holding_with(
                "UTMA",
                "custodial",
                "EUR",
                json!({"beneficiary": "Child A"}),
            )
            .await;
        self.balance(utma, "2026-10-26", 100_000).await;
        let giving = self
            .holding_with("Giving", "charitable", "USD", json!({}))
            .await;
        self.balance(giving, "2026-10-28", 500_000).await;
        self.display("USD").await;
    }
}

/// The guide's first example, keys and figures, is what that household is answered.
#[tokio::test]
async fn the_guide_s_example_is_what_the_report_answers() {
    let (_, after) = GUIDE.split_once("```json\n").unwrap();
    let (example, _) = after.split_once("```").unwrap();
    let documented: Value = serde_json::from_str(example).unwrap();

    let world = world().await;
    world.the_guide_s_household().await;
    let answered = world.report("from=2026-10-01&interval=month").await;

    assert_agrees(&documented, &answered, "report");
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
