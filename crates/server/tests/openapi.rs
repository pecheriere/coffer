//! The generated OpenAPI document: it matches the committed snapshot, the
//! router serves exactly what it documents, and every schema and error is
//! typed.

#![expect(
    clippy::unwrap_used,
    reason = "integration test: helpers outside #[test] functions panic to fail the test"
)]

use std::ffi::OsString;

use axum::body::Body;
use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use coffer_domain::identity::{Actor, Role, Scope, TokenName, Via};
use coffer_domain::ids::MemberId;
use coffer_server::cli;
use coffer_server::http::api::openapi::document;
use coffer_server::http::router;
use coffer_server::secrets::SecretToken;
use coffer_server::testkit::TestServer;
use http_body_util::BodyExt as _;
use secrecy::ExposeSecret as _;
use serde_json::Value;
use tower::ServiceExt as _;

const SNAPSHOT: &str = include_str!("../../../docs/api/openapi.json");
const MATCHED: HeaderName = HeaderName::from_static("x-test-matched-path");

/// The OpenAPI document as JSON.
fn document_json() -> Value {
    serde_json::to_value(document()).unwrap()
}

/// Every `(path, method, operation)` the document declares.
fn operations(doc: &Value) -> Vec<(String, Method, Value)> {
    let mut found = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if let Ok(method) = method.to_uppercase().parse::<Method>() {
                found.push((path.clone(), method, operation.clone()));
            }
        }
    }
    found
}

/// An admin with a read token.
async fn server_with_token() -> (TestServer, SecretToken) {
    let server = TestServer::new().await;
    let link = server.identity.bootstrap_link().await.unwrap().unwrap();
    let member = server.authenticator().enroll(&link, "alice", "Alice").await;
    let alice = Actor {
        member,
        role: Role::Admin,
        via: Via::Session,
    };
    let (_, token) = server
        .agent_tokens
        .create(
            &alice,
            TokenName::parse("claude").unwrap(),
            Scope::Read,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    (server, token)
}

/// `path` with every `{name}` parameter replaced by a valid value: a date
/// for `format: date` parameters, a fresh `UUIDv7` otherwise.
fn concrete(path: &str, operation: &Value) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}').unwrap() + start;
        let name = &rest[start + 1..end];
        let is_date = operation["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| p["name"] == name && p["schema"]["format"] == "date");
        out.push_str(&rest[..start]);
        if is_date {
            out.push_str("2026-01-15");
        } else {
            out.push_str(&MemberId::new().to_string());
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

async fn copy_matched_path(request: Request, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let mut response = next.run(request).await;
    if let Some(matched) = matched {
        response
            .headers_mut()
            .insert(MATCHED, HeaderValue::from_str(&matched).unwrap());
    }
    response
}

#[tokio::test]
async fn snapshot_matches_generated_document() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::run(
        ["coffer", "openapi"].map(OsString::from),
        |_| None,
        &mut out,
        &mut err,
    )
    .await;

    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(String::from_utf8(err).unwrap(), "");
    assert!(
        String::from_utf8(out).unwrap() == SNAPSHOT,
        "docs/api/openapi.json is stale; regenerate it with \
         `cargo run -p coffer-server -- openapi > docs/api/openapi.json`"
    );
}

#[tokio::test]
async fn every_documented_operation_is_routed() {
    let (server, token) = server_with_token().await;
    let doc = document_json();
    let operations = operations(&doc);
    assert_ne!(operations.len(), 0);

    for (path, method, operation) in operations {
        let uri = concrete(&path, &operation);
        let request = Request::builder()
            .method(method.clone())
            .uri(&uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.expose_secret()),
            )
            .body(Body::empty())
            .unwrap();
        let response = router(server.app_state())
            .layer(middleware::from_fn(copy_matched_path))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get(&MATCHED)
                .map(|v| v.to_str().unwrap()),
            Some(path.as_str()),
            "{method} {uri}"
        );
        assert_ne!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn documented_methods_match_routed_methods() {
    let (server, _) = server_with_token().await;
    let doc = document_json();

    for (path, item) in doc["paths"].as_object().unwrap() {
        let mut documented: Vec<String> = item
            .as_object()
            .unwrap()
            .keys()
            .filter_map(|method| method.to_uppercase().parse::<Method>().ok())
            .map(|method| method.to_string())
            .collect();
        if documented.iter().any(|method| method == "GET") {
            documented.push("HEAD".to_owned());
        }
        documented.sort();

        let uri = concrete(path, &item["get"]);
        let response = router(server.app_state())
            .oneshot(
                Request::builder()
                    .method(Method::TRACE)
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        let mut allowed: Vec<String> = response.headers()[header::ALLOW]
            .to_str()
            .unwrap()
            .split(',')
            .map(|method| method.trim().to_owned())
            .collect();
        allowed.sort();
        assert_eq!(allowed, documented, "{path}");
    }
}

/// Every schema position in `doc`, with a label saying where it is.
fn schemas(doc: &Value) -> Vec<(String, Value)> {
    fn walk(at: &str, schema: &Value, found: &mut Vec<(String, Value)>) {
        found.push((at.to_owned(), schema.clone()));
        if let Some(properties) = schema["properties"].as_object() {
            for (name, property) in properties {
                walk(&format!("{at}.properties.{name}"), property, found);
            }
        }
        for key in ["items", "not"] {
            if schema[key].is_object() {
                walk(&format!("{at}.{key}"), &schema[key], found);
            }
        }
        if schema["additionalProperties"].is_object() {
            walk(
                &format!("{at}.additionalProperties"),
                &schema["additionalProperties"],
                found,
            );
        }
        for key in ["oneOf", "anyOf", "allOf", "prefixItems"] {
            for (index, member) in schema[key].as_array().into_iter().flatten().enumerate() {
                walk(&format!("{at}.{key}[{index}]"), member, found);
            }
        }
    }

    let mut found = Vec::new();
    for (name, schema) in doc["components"]["schemas"]
        .as_object()
        .into_iter()
        .flatten()
    {
        walk(&format!("components.{name}"), schema, &mut found);
    }
    for (path, method, operation) in operations(doc) {
        let at = format!("{method} {path}");
        for parameter in operation["parameters"].as_array().into_iter().flatten() {
            let name = parameter["name"].as_str().unwrap();
            walk(
                &format!("{at} parameter {name}"),
                &parameter["schema"],
                &mut found,
            );
        }
        for (media, content) in operation["requestBody"]["content"]
            .as_object()
            .into_iter()
            .flatten()
        {
            walk(
                &format!("{at} request {media}"),
                &content["schema"],
                &mut found,
            );
        }
        for (status, response) in operation["responses"].as_object().unwrap() {
            for (media, content) in response["content"].as_object().into_iter().flatten() {
                walk(
                    &format!("{at} {status} {media}"),
                    &content["schema"],
                    &mut found,
                );
            }
        }
    }
    found
}

#[test]
fn no_untyped_schemas() {
    let doc = document_json();
    let schemas = schemas(&doc);
    assert_ne!(schemas.len(), 0);
    for (at, schema) in schemas {
        let typed = ["type", "$ref", "oneOf", "anyOf", "allOf"]
            .iter()
            .any(|key| schema.get(key).is_some());
        assert!(typed, "untyped schema at {at}: {schema}");
    }
}

#[test]
fn every_error_response_is_problem_json() {
    let doc = document_json();
    assert!(doc["components"]["schemas"]["ProblemBody"].is_object());
    for (path, method, operation) in operations(&doc) {
        for (status, response) in operation["responses"].as_object().unwrap() {
            if status.starts_with('2') {
                continue;
            }
            let content = response["content"].as_object().unwrap();
            let media: Vec<&String> = content.keys().collect();
            assert_eq!(
                media,
                ["application/problem+json"],
                "{method} {path} {status}"
            );
            assert_eq!(
                content["application/problem+json"]["schema"]["$ref"],
                "#/components/schemas/ProblemBody",
                "{method} {path} {status}"
            );
        }
    }
}

#[tokio::test]
async fn openapi_served_without_auth() {
    let server = TestServer::new().await;

    let response = router(server.app_state())
        .oneshot(
            Request::get("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body, document_json());
}

#[test]
fn every_operation_but_the_document_requires_a_bearer_token() {
    let doc = document_json();
    assert!(doc["components"]["securitySchemes"]["bearer"].is_object());
    for (path, method, operation) in operations(&doc) {
        let security = &operation["security"];
        if path == "/api/v1/openapi.json" {
            assert!(
                security.is_null() || security == &serde_json::json!([]),
                "{method} {path}"
            );
        } else {
            assert_eq!(
                security,
                &serde_json::json!([{ "bearer": [] }]),
                "{method} {path}"
            );
        }
    }
}
