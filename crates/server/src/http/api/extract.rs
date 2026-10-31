//! Typed request extractors for the JSON API. Every rejection is a
//! `400 invalid` problem with a fixed detail that names at most a
//! documented parameter: never a supplied value or an undocumented name.
//! A rejection is logged at `debug` with where it happened (a path of
//! documented names, anything else shown as `?`) and a category, never the
//! parser's message: it may quote input, and input may be a credential.

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, RawPathParams, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use serde::de::DeserializeOwned;
use serde_path_to_error::Segment;
use utoipa::openapi::path::ParameterIn;
use utoipa::{IntoParams, ToSchema};

use crate::http::problem::Problem;

const BODY_INVALID: &str = "the request body is not valid JSON for this operation";
const QUERY_UNRECOGNISED: &str = "a query parameter is not recognised or is repeated";
const QUERY_INVALID: &str = "the query string is not valid for this operation";
const PATH_INVALID: &str = "a path parameter is not valid";

/// A JSON request body deserialized into `T`.
#[derive(Debug, Clone)]
pub struct TypedJson<T>(pub T);

/// A JSON request body deserialized into `T`, with the raw bytes it was read
/// from, for idempotent creates that hash the request. Rejected exactly like
/// [`TypedJson`].
#[derive(Debug, Clone)]
pub struct TypedJsonBytes<T> {
    pub value: T,
    pub bytes: Bytes,
}

/// A query string deserialized into `T`, a struct with
/// `#[serde(deny_unknown_fields)]`, so unknown and repeated parameters are
/// rejected.
#[derive(Debug, Clone)]
pub struct TypedQuery<T>(pub T);

/// Path parameters deserialized into `T`, a struct with one field per
/// parameter.
#[derive(Debug, Clone)]
pub struct TypedPath<T>(pub T);

impl<T, S> FromRequest<S> for TypedJson<T>
where
    T: DeserializeOwned + ToSchema,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let TypedJsonBytes { value, .. } = TypedJsonBytes::from_request(request, state).await?;
        Ok(Self(value))
    }
}

impl<T, S> FromRequest<S> for TypedJsonBytes<T>
where
    T: DeserializeOwned + ToSchema,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json(request.headers()) {
            return Err(body_rejected(".", "content_type"));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| {
                let category = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    "too_large"
                } else {
                    "unreadable"
                };
                body_rejected(".", category)
            })?;
        let value = parse_json::<T>(&bytes)?;
        Ok(Self { value, bytes })
    }
}

/// Whether the `Content-Type` is `application/json` or `application/*+json`.
fn is_json(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.split_once('/').is_some_and(|(kind, subtype)| {
        kind == "application" && (subtype == "json" || subtype.ends_with("+json"))
    })
}

/// Deserializes the JSON `bytes` into `T`, trailing content refused.
fn parse_json<T: DeserializeOwned + ToSchema>(bytes: &[u8]) -> Result<T, Problem> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    match serde_path_to_error::deserialize::<_, T>(&mut deserializer) {
        Ok(value) => match deserializer.end() {
            Ok(()) => Ok(value),
            Err(error) => Err(body_rejected(".", json_category(&error))),
        },
        Err(error) => {
            let names = schema_names::<T>();
            let path = loggable_path(error.path(), |name| names.iter().any(|known| known == name));
            Err(body_rejected(&path, json_category(error.inner())))
        }
    }
}

/// Logs a body rejection and returns its problem.
fn body_rejected(path: &str, category: &'static str) -> Problem {
    tracing::debug!(path = %path, category = %category, "request body rejected");
    Problem::invalid_request(BODY_INVALID)
}

fn json_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

/// The property names and enumerated string values `T`'s schema documents,
/// with those of every schema it references.
fn schema_names<T: ToSchema>() -> Vec<String> {
    fn walk(value: &serde_json::Value, names: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(serde_json::Value::Object(properties)) = object.get("properties") {
                    names.extend(properties.keys().cloned());
                }
                if let Some(serde_json::Value::Array(values)) = object.get("enum") {
                    names.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
                }
                object.values().for_each(|nested| walk(nested, names));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, names)),
            _ => {}
        }
    }
    let mut schemas = vec![(T::name().into_owned(), T::schema())];
    T::schemas(&mut schemas);
    let mut names = Vec::new();
    for (_, schema) in &schemas {
        // A schema always serializes; one that did not would only shorten
        // the logged path.
        if let Ok(value) = serde_json::to_value(schema) {
            walk(&value, &mut names);
        }
    }
    names
}

/// `path` with every name `documented` does not accept shown as `?`, so a
/// log line never carries a name the client made up.
fn loggable_path(path: &serde_path_to_error::Path, documented: impl Fn(&str) -> bool) -> String {
    let mut out = String::new();
    for segment in path {
        let name = match segment {
            Segment::Seq { index } => {
                out.push('[');
                out.push_str(&index.to_string());
                out.push(']');
                continue;
            }
            Segment::Map { key } | Segment::Enum { variant: key } if documented(key) => key,
            Segment::Map { .. } | Segment::Enum { .. } | Segment::Unknown => "?",
        };
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(name);
    }
    if out.is_empty() {
        out.push('.');
    }
    out
}

impl<T, S> FromRequestParts<S> for TypedQuery<T>
where
    T: DeserializeOwned + IntoParams + Send,
    S: Send + Sync,
{
    type Rejection = Problem;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(parse_query::<T>(parts.uri.query().unwrap_or_default()).map(Self))
    }
}

impl<T, S> FromRequestParts<S> for TypedPath<T>
where
    T: DeserializeOwned + IntoParams,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|rejection| {
                tracing::debug!(
                    status = rejection.status().as_u16(),
                    "path parameters rejected"
                );
                Problem::invalid_request(PATH_INVALID)
            })?;
        // Re-encoded so that path and query parameters parse alike, scalars
        // included.
        let mut encoded = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &params {
            encoded.append_pair(name, value);
        }
        parse::<T>(&encoded.finish(), &ParameterIn::Path, PATH_INVALID).map(Self)
    }
}

/// Deserializes `query` into `T`, refusing unknown and repeated names before
/// `T` sees them.
fn parse_query<T: DeserializeOwned + IntoParams>(query: &str) -> Result<T, Problem> {
    let documented = documented::<T>(&ParameterIn::Query);
    let mut seen: Vec<String> = Vec::new();
    for (name, _) in url::form_urlencoded::parse(query.as_bytes()) {
        if !documented.iter().any(|known| *known == *name) || seen.iter().any(|s| *s == *name) {
            return Err(Problem::invalid_request(QUERY_UNRECOGNISED));
        }
        seen.push(name.into_owned());
    }
    parse::<T>(query, &ParameterIn::Query, QUERY_INVALID)
}

/// Deserializes the `application/x-www-form-urlencoded` `encoded` into `T`.
/// A failure names the parameter at fault when `T` documents it at
/// `location`, and is `fallback` otherwise.
fn parse<T: DeserializeOwned + IntoParams>(
    encoded: &str,
    location: &ParameterIn,
    fallback: &str,
) -> Result<T, Problem> {
    let deserializer =
        serde_urlencoded::Deserializer::new(url::form_urlencoded::parse(encoded.as_bytes()));
    serde_path_to_error::deserialize(deserializer).map_err(|error| {
        let documented = documented::<T>(location);
        let path = loggable_path(error.path(), |name| {
            documented.iter().any(|known| known == name)
        });
        tracing::debug!(path = %path, "parameters rejected");
        match named(&documented, &error.path().to_string()) {
            Some(detail) => Problem::invalid_request(&detail),
            None => Problem::invalid_request(fallback),
        }
    })
}

/// The names of `T`'s documented parameters.
fn documented<T: IntoParams>(location: &ParameterIn) -> Vec<String> {
    T::into_params(|| Some(location.clone()))
        .into_iter()
        .map(|parameter| parameter.name)
        .collect()
}

/// The detail naming `name`, if it is documented.
fn named(documented: &[String], name: &str) -> Option<String> {
    documented
        .iter()
        .any(|known| known == name)
        .then(|| format!("the `{name}` parameter is not valid"))
}
