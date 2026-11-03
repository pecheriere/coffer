//! A tool's JSON Schemas, read from the `OpenAPI` document, so a tool can't describe an
//! operation differently from the API. `OpenAPI` 3.1 schemas are JSON Schema 2020-12, the
//! dialect MCP assumes. All that changes is where they live: a tool's schema has to stand
//! alone, so what it refers to moves under its own `$defs`.

use std::sync::LazyLock;

use rmcp::model::JsonObject;
use serde_json::{Map, Value, json};
use utoipa::ToSchema;

use crate::http::api::openapi;

const COMPONENT_PREFIX: &str = "#/components/schemas/";
const DEFS_PREFIX: &str = "#/$defs/";

/// A schema the document doesn't hold: a bug in the tool table, which a test catches.
#[derive(Debug, Clone, thiserror::Error)]
#[error("the OpenAPI document has no {0}")]
pub(super) struct MissingFromDocument(pub String);

static DOCUMENT: LazyLock<Value> =
    LazyLock::new(|| serde_json::to_value(openapi::document()).unwrap_or(Value::Null));

/// One operation of the document, by its `operationId`.
pub(super) struct Operation {
    body: &'static Value,
    id: &'static str,
}

impl Operation {
    pub(super) fn find(id: &'static str) -> Result<Self, MissingFromDocument> {
        DOCUMENT["paths"]
            .as_object()
            .into_iter()
            .flat_map(Map::values)
            .filter_map(Value::as_object)
            .flat_map(Map::values)
            .find(|operation| operation["operationId"] == id)
            .map(|body| Self { body, id })
            .ok_or_else(|| MissingFromDocument(format!("operation {id}")))
    }

    /// What the document says this operation does.
    pub(super) fn description(&self) -> &'static str {
        self.body["description"].as_str().unwrap_or_default()
    }

    /// The tool's arguments: every path and query parameter as a property, or, for an
    /// operation taking a body, the body's own schema. `omit` names parameters the tool
    /// fills in itself.
    pub(super) fn input(&self, omit: Option<&str>) -> Result<JsonObject, MissingFromDocument> {
        let body = &self.body["requestBody"]["content"]["application/json"]["schema"];
        if !body.is_null() {
            return standalone(body.clone());
        }
        let mut properties = Map::new();
        let mut required = Vec::new();
        for parameter in self.body["parameters"].as_array().into_iter().flatten() {
            let Some(name) = parameter["name"].as_str() else {
                continue;
            };
            if parameter["in"] == "header" || omit == Some(name) {
                continue;
            }
            let mut schema = parameter["schema"].clone();
            if let (Some(schema), Some(description)) =
                (schema.as_object_mut(), parameter.get("description"))
            {
                schema.insert("description".to_owned(), description.clone());
            }
            properties.insert(name.to_owned(), schema);
            if parameter["required"] == true {
                required.push(name);
            }
        }
        standalone(json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }))
    }

    /// The schema of what the tool answers, the operation's `200`.
    pub(super) fn output(&self) -> Result<JsonObject, MissingFromDocument> {
        let schema = &self.body["responses"]["200"]["content"]["application/json"]["schema"];
        if schema.is_null() {
            return Err(MissingFromDocument(format!("answer of {}", self.id)));
        }
        standalone(schema.clone())
    }
}

/// The arguments of a tool narrower than its operation, from a type of the tool's own.
pub(super) fn input_of<T: ToSchema>() -> Result<JsonObject, MissingFromDocument> {
    let mut schemas = Vec::new();
    T::schemas(&mut schemas);
    let own = schemas
        .into_iter()
        .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap_or(Value::Null)))
        .collect();
    standalone_with(
        serde_json::to_value(T::schema()).unwrap_or(Value::Null),
        &own,
    )
}

/// `schema` as a schema of its own: an object at its root, as MCP asks, with what it
/// refers to under `$defs`.
fn standalone(schema: Value) -> Result<JsonObject, MissingFromDocument> {
    standalone_with(schema, &Map::new())
}

/// As [`standalone`], with `own` schemas beside the document's.
fn standalone_with(
    schema: Value,
    own: &Map<String, Value>,
) -> Result<JsonObject, MissingFromDocument> {
    let component = |name: &str| {
        own.get(name)
            .or_else(|| DOCUMENT["components"]["schemas"].get(name))
            .ok_or_else(|| MissingFromDocument(format!("schema {name}")))
    };
    let mut root = match component_named(&schema) {
        Some(name) => component(name)?.clone(),
        None => schema,
    };
    let mut wanted = Vec::new();
    point_at_defs(&mut root, &mut wanted);
    let mut defs = Map::new();
    while let Some(name) = wanted.pop() {
        if !defs.contains_key(&name) {
            let mut schema = component(&name)?.clone();
            point_at_defs(&mut schema, &mut wanted);
            defs.insert(name, schema);
        }
    }
    let Value::Object(mut root) = root else {
        return Err(MissingFromDocument("object schema".to_owned()));
    };
    if !defs.is_empty() {
        root.insert("$defs".to_owned(), Value::Object(defs));
    }
    Ok(root)
}

/// The component `schema` is nothing but a reference to.
fn component_named(schema: &Value) -> Option<&str> {
    schema.get("$ref")?.as_str()?.strip_prefix(COMPONENT_PREFIX)
}

/// Points every reference in `value` under `$defs`, adding its name to `wanted`.
fn point_at_defs(value: &mut Value, wanted: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, nested) in object.iter_mut() {
                if key == "$ref" {
                    if let Some(name) = nested
                        .as_str()
                        .and_then(|text| text.strip_prefix(COMPONENT_PREFIX))
                    {
                        wanted.push(name.to_owned());
                        *nested = Value::String(format!("{DEFS_PREFIX}{name}"));
                    }
                } else {
                    point_at_defs(nested, wanted);
                }
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| point_at_defs(item, wanted)),
        _ => {}
    }
}
