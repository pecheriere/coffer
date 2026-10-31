//! Typed request extractors for the JSON API. Every rejection is a
//! `400 invalid` problem with a fixed detail that names at most a
//! documented parameter: never a supplied value or an undocumented name.
//! The parser's own message, which may quote input, is logged at `debug`
//! only.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Json, RawPathParams, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;
use utoipa::IntoParams;
use utoipa::openapi::path::ParameterIn;

use crate::http::problem::Problem;

const BODY_INVALID: &str = "the request body is not valid JSON for this operation";
const QUERY_UNRECOGNISED: &str = "a query parameter is not recognised or is repeated";
const QUERY_INVALID: &str = "the query string is not valid for this operation";
const PATH_INVALID: &str = "a path parameter is not valid";

/// A JSON request body deserialized into `T`.
#[derive(Debug, Clone)]
pub struct TypedJson<T>(pub T);

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
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let rejection: JsonRejection = rejection;
                tracing::debug!(error = %rejection.body_text(), "request body rejected");
                Err(Problem::invalid_request(BODY_INVALID))
            }
        }
    }
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
                tracing::debug!(error = %rejection.body_text(), "path parameters rejected");
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
        tracing::debug!(error = %error.inner(), "parameters rejected");
        let documented = documented::<T>(location);
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
