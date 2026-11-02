//! The OpenAPI 3.1 document of the JSON API, generated from the routes, and
//! `GET /api/v1/openapi.json`, which serves it without authentication.

use std::borrow::Cow;
use std::sync::LazyLock;

use axum::Json;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::server::{ServerBuilder, ServerVariableBuilder};
use utoipa::openapi::tag::TagBuilder;
use utoipa::openapi::{
    ComponentsBuilder, ContactBuilder, InfoBuilder, LicenseBuilder, OpenApi, OpenApiBuilder, RefOr,
};
use utoipa::{PartialSchema as _, ToSchema};

use crate::http::problem::ProblemBody;

/// The security scheme every operation but this document's requires.
pub(super) const BEARER: &str = "bearer";

/// Operation tags; [`base`] describes each.
pub(super) mod tags {
    pub const IDENTITY: &str = "identity";
    pub const MEMBERS: &str = "members";
    pub const ACCOUNTS: &str = "accounts";
    pub const CATEGORIES: &str = "categories";
    pub const TRANSACTIONS: &str = "transactions";
    pub const PLAN: &str = "plan";
    pub const CONNECTIONS: &str = "connections";
    pub const AUDIT: &str = "audit";
    pub const META: &str = "meta";
}

static DOCUMENT: LazyLock<OpenApi> = LazyLock::new(|| super::routes().split().1);

/// The OpenAPI document of every `/api/v1` operation.
#[must_use]
pub fn document() -> OpenApi {
    DOCUMENT.clone()
}

/// The document before any operation is added: info, server, tags and the
/// bearer security scheme.
pub(super) fn base() -> OpenApi {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let info = InfoBuilder::new()
        .title("coffer")
        .version(env!("CARGO_PKG_VERSION"))
        .description(Some(
            "The JSON API of a coffer server, a self-hosted household finance hub. \
             Every operation but this document's takes an agent token as \
             `Authorization: Bearer coffer_…`. Errors are `application/problem+json` \
             bodies whose `type` is `urn:coffer:problem:<code>`.",
        ))
        .contact(Some(
            ContactBuilder::new()
                .name(Some("coffer"))
                .url(Some(repository))
                .build(),
        ))
        .license(Some(
            LicenseBuilder::new()
                .name(env!("CARGO_PKG_LICENSE"))
                .url(Some(format!("{repository}#license")))
                .build(),
        ))
        .build();
    let server = ServerBuilder::new()
        .url("{origin}")
        .description(Some("A coffer server"))
        .parameter(
            "origin",
            ServerVariableBuilder::new()
                .default_value("http://localhost:8080")
                .description(Some("The server's `COFFER_PUBLIC_URL`")),
        )
        .build();
    let tag = |name: &str, description: &str| {
        TagBuilder::new()
            .name(name)
            .description(Some(description))
            .build()
    };
    // Error responses come from `CommonProblems`, whose schemas no route
    // registers.
    let mut problem_schemas = vec![(ProblemBody::name().into_owned(), ProblemBody::schema())];
    <ProblemBody as ToSchema>::schemas(&mut problem_schemas);
    let components = ComponentsBuilder::new()
        .schemas_from_iter(problem_schemas)
        .security_scheme(
            BEARER,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("coffer agent token")
                    .description(Some(
                        "An agent token, created by a member on their account page.",
                    ))
                    .build(),
            ),
        )
        .build();
    OpenApiBuilder::new()
        .info(info)
        .servers(Some([server]))
        .tags(Some([
            tag(tags::IDENTITY, "Who the token belongs to."),
            tag(tags::MEMBERS, "The household's members."),
            tag(
                tags::ACCOUNTS,
                "Accounts and their daily balances. Amounts are integer minor units of the \
                 account's currency.",
            ),
            tag(
                tags::CATEGORIES,
                "The household's category groups and categories, which transaction splits \
                 use.",
            ),
            tag(
                tags::TRANSACTIONS,
                "Transactions, their splits into categories, and their review: listing, \
                 search, single changes and atomic batches.",
            ),
            tag(
                tags::PLAN,
                "The household's spending plan: recurring items, one-offs and goals. Amounts \
                 are positive magnitudes with a direction.",
            ),
            tag(
                tags::CONNECTIONS,
                "Connections to data providers, their syncs, and the provider accounts they \
                 report, which are bound to accounts.",
            ),
            tag(tags::AUDIT, "The append-only log of every change."),
            tag(tags::META, "This API's own description."),
        ]))
        .components(Some(components))
        .build()
}

/// The schema of this document in its own `responses`: an OpenAPI 3.1
/// object, which the OpenAPI specification itself describes.
struct OpenApiDocument;

impl utoipa::PartialSchema for OpenApiDocument {
    fn schema() -> RefOr<Schema> {
        let string = || ObjectBuilder::new().schema_type(Type::String);
        let object = || ObjectBuilder::new().schema_type(Type::Object);
        ObjectBuilder::new()
            .schema_type(Type::Object)
            .description(Some(
                "An OpenAPI 3.1 document (https://spec.openapis.org/oas/v3.1.0).",
            ))
            .property("openapi", string())
            .property("info", object())
            .property("servers", ArrayBuilder::new().items(object()))
            .property("paths", object())
            .property("components", object())
            .property("tags", ArrayBuilder::new().items(object()))
            .required("openapi")
            .required("info")
            .into()
    }
}

impl ToSchema for OpenApiDocument {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("OpenApiDocument")
    }
}

/// The OpenAPI document of this API.
#[utoipa::path(
    get,
    path = "/api/v1/openapi.json",
    operation_id = "getOpenApi",
    tag = tags::META,
    description = "The OpenAPI 3.1 document describing every operation of this API. \
                   Needs no token.",
    responses(
        (status = 200, description = "The OpenAPI document.", body = OpenApiDocument),
    ),
)]
pub(super) async fn serve() -> Json<OpenApi> {
    Json(document())
}
