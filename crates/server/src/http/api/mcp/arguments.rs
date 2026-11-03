//! A tool call's arguments, read into the very types the API's handlers take, with the
//! API's rule for a rejection: fixed text naming at most a documented parameter, never
//! what was sent.

use rmcp::model::JsonObject;
use serde::de::DeserializeOwned;
use serde_json::Value;
use utoipa::openapi::path::ParameterIn;
use utoipa::{IntoParams, ToSchema};

use crate::http::api::extract::{documented, loggable_path, named, parse_json};
use crate::http::problem::Problem;

const NOT_RECOGNISED: &str = "an argument is not recognised";
const NOT_VALID: &str = "the arguments are not valid for this tool";

/// What is left of a call's arguments.
#[derive(Debug, Default)]
pub(super) struct Arguments(JsonObject);

impl Arguments {
    pub(super) fn new(arguments: Option<JsonObject>) -> Self {
        Self(arguments.unwrap_or_default())
    }

    /// Takes the arguments `T` documents and reads them into it.
    pub(super) fn take<T>(&mut self, location: &ParameterIn) -> Result<T, Problem>
    where
        T: DeserializeOwned + IntoParams,
    {
        let names = documented::<T>(location);
        let mut taken = JsonObject::new();
        for name in &names {
            if let Some(value) = self.0.remove(name) {
                taken.insert(name.clone(), value);
            }
        }
        serde_path_to_error::deserialize(Value::Object(taken)).map_err(|error| {
            let path = loggable_path(error.path(), |name| names.iter().any(|known| known == name));
            tracing::debug!(path = %path, "tool arguments rejected");
            match named(&names, &error.path().to_string()) {
                Some(detail) => Problem::invalid_request(&detail),
                None => Problem::invalid_request(NOT_VALID),
            }
        })
    }

    /// Refuses whatever no earlier [`Self::take`] asked for.
    pub(super) fn finish(self) -> Result<(), Problem> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Problem::invalid_request(NOT_RECOGNISED))
        }
    }

    /// Reads all the arguments as `T`, an operation's request body.
    pub(super) fn body<T: DeserializeOwned + ToSchema>(self) -> Result<T, Problem> {
        let bytes = serde_json::to_vec(&Value::Object(self.0))
            .map_err(|_| Problem::invalid_request(NOT_VALID))?;
        parse_json(&bytes)
    }

    /// Whether `name` was given.
    pub(super) fn has(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    /// Fills in `name`.
    pub(super) fn set(&mut self, name: &str, value: &str) {
        self.0
            .insert(name.to_owned(), Value::String(value.to_owned()));
    }
}
