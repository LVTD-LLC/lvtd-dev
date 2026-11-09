//! The JSON API, mounted at `/api/v1` when the `api` feature is enabled.
//!
//! Requests authenticate with a personal API key (`Authorization: Bearer
//! sr_...`) that users create on the settings page, or with an OAuth 2.1
//! access token (`sro_...`) from [`crate::oauth_server`]. Browser sessions are
//! never accepted here, so cookies cannot be replayed cross-site, and no
//! CORS headers are sent. Every response is JSON, including errors
//! (`{"error": "..."}`) and the 404 for unknown paths.
//!
//! Two endpoints ship with every project: `GET /api/v1` describes the
//! service and `GET /api/v1/me` returns the account behind the key. Add
//! your product's endpoints to [`documented_routes`]: annotate the handler
//! with `#[utoipa::path]`, take [`ApiUser`] as an argument to require a key,
//! return `Result<Response, ApiError>`, and build bodies with [`json_body`]
//! from a `ToSchema` struct. Keep business logic in your own modules and
//! call in from here, as the HTML handlers do.
//!
//! The OpenAPI document (`/openapi.json` and `/api/v1/openapi.json`) and its Swagger UI
//! (`/api/v1/docs`) are generated from those annotations and types, so a
//! new endpoint is documented the moment it is registered.

use crate::{AppError, SharedState, api_keys};
use app_auth::User;
use axum::Router;
use axum::extract::{FromRequestParts, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::LazyLock;
use time::format_description::well_known::Rfc3339;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const BASE_PATH: &str = "/api/v1";
/// The OpenAPI document, mounted at the root and relative to [`BASE_PATH`].
pub const OPENAPI_PATH: &str = "/openapi.json";
/// The Swagger UI page for the document, relative to [`BASE_PATH`].
pub const SWAGGER_PATH: &str = "/docs";

pub fn router() -> Router<SharedState> {
    let (router, _) = documented_routes().split_for_parts();
    router.route(OPENAPI_PATH, get(openapi_json)).route(SWAGGER_PATH, get(swagger_ui)).fallback(not_found)
}

/// Every endpoint of the API. A handler registered here with `routes!` and
/// annotated with `#[utoipa::path]` is served and documented in one step;
/// a route added with plain `.route` is served but missing from the
/// OpenAPI document. Handlers sharing a path go in the same `routes!`.
fn documented_routes() -> OpenApiRouter<SharedState> {
    OpenApiRouter::new().routes(routes!(info)).routes(routes!(me))
}

// ---------------------------------------------------------------------------
// OpenAPI document and Swagger UI
// ---------------------------------------------------------------------------

/// The security scheme name; `#[utoipa::path(security(...))]` needs it as a literal.
const BEARER: &str = "api_key";

#[derive(OpenApi)]
#[openapi(
    info(description = "Send a personal API key from Settings as `Authorization: Bearer sr_...`. \
        Every error is a JSON object with an `error` message."),
    modifiers(&Document),
    tags((name = "service", description = "This service"), (name = "account", description = "The account behind the key")),
)]
struct ApiDoc;

/// Runtime parts of the document: the product name and the bearer scheme.
struct Document;

impl utoipa::Modify for Document {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.info.title = format!("{} API", crate::views::SITE_NAME);
        openapi.info.version = env!("CARGO_PKG_VERSION").to_owned();
        openapi.info.license = None;
        openapi.info.contact = None;
        let mut scheme = Http::new(HttpAuthScheme::Bearer);
        scheme.bearer_format = Some("sr_<64 hex characters>".to_owned());
        scheme.description = Some(
            "A personal API key from Settings, or an OAuth 2.1 access token obtained through the authorization server \
             described at /.well-known/oauth-authorization-server. Cookies are never accepted."
                .to_owned(),
        );
        openapi.components.get_or_insert_default().add_security_scheme(BEARER, SecurityScheme::Http(scheme));
    }
}

/// The OpenAPI 3.1 document for the API, built once from the handlers' annotations.
pub fn openapi() -> &'static str {
    static DOCUMENT: LazyLock<String> = LazyLock::new(|| {
        OpenApiRouter::with_openapi(ApiDoc::openapi())
            .nest(BASE_PATH, documented_routes())
            .into_openapi()
            .to_json()
            .expect("the OpenAPI document serializes")
    });
    &DOCUMENT
}

/// Public: the document is the same for every caller and changes only with a deploy.
pub(crate) async fn openapi_json() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")],
        openapi(),
    )
        .into_response()
}

/// Public: Swagger UI for [`openapi`], with the pinned bundle from the CDN.
async fn swagger_ui(State(state): State<SharedState>) -> Response {
    crate::routes::html(StatusCode::OK, crate::views::api_docs::swagger(&state, &format!("{BASE_PATH}{OPENAPI_PATH}")))
}

// ---------------------------------------------------------------------------
// Responses and errors
// ---------------------------------------------------------------------------

/// A JSON body with the headers every API response carries.
pub fn json_response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "private, no-store")],
        value.to_string(),
    )
        .into_response()
}

/// A `ToSchema` body as JSON, with the headers every API response carries.
/// Serialize the same type the handler's `#[utoipa::path]` documents.
pub fn json_body(status: StatusCode, body: &impl Serialize) -> Response {
    json_response(status, serde_json::to_value(body).expect("API bodies serialize"))
}

/// The body of every error response.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// What went wrong, written for a person.
    #[schema(example = "This API key is invalid or has been revoked.")]
    pub error: String,
}

/// Like [`AppError`], but every outcome is a JSON object with an `error` message.
#[derive(Debug)]
pub enum ApiError {
    Status(StatusCode, String),
    Internal(anyhow::Error),
}

impl ApiError {
    pub fn status(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Status(status, message.into())
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Internal(error) => Self::Internal(error),
            AppError::Status(status, message) => Self::Status(status, message),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            // Route through AppError so logging and error reporting stay in one place,
            // then replace the plain-text body with JSON.
            Self::Internal(error) => {
                let mut response = AppError::Internal(error).into_response();
                let (parts, _) = response.into_parts();
                response = json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "internal error" }));
                *response.extensions_mut() = parts.extensions;
                response
            }
            Self::Status(status, message) => json_body(status, &ErrorBody { error: message }),
        }
    }
}

async fn not_found() -> ApiError {
    ApiError::status(StatusCode::NOT_FOUND, "Not found.")
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// The owner of the bearer credential, an API key or an OAuth access
/// token. Rejects with a JSON 401; never reads cookies.
pub struct ApiUser(pub User);

/// The realm named in `WWW-Authenticate`; the resource metadata next to it
/// is how MCP clients discover the OAuth server.
const REALM: &str = env!("CARGO_PKG_NAME");

fn unauthorized(state: &SharedState, path: &str, message: &str) -> Response {
    let mut response = json_response(StatusCode::UNAUTHORIZED, json!({ "error": message }));
    let resource = if cfg!(feature = "mcp") && path.starts_with("/mcp") { "/mcp" } else { "" };
    response.headers_mut().insert(header::WWW_AUTHENTICATE, state.oauth_server.www_authenticate(REALM, resource));
    response
}

impl FromRequestParts<SharedState> for ApiUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim());
        let path = parts.uri.path().to_owned();
        let Some(token) = token else {
            return Err(unauthorized(
                state,
                &path,
                "Provide an API key as `Authorization: Bearer sr_...` (create one in Settings) or connect with OAuth.",
            ));
        };
        let user = if token.starts_with(app_auth::oauth_server::ACCESS_PREFIX) {
            app_auth::oauth_server::authenticate(&state.pool, token).await.map_err(ApiError::from)
        } else {
            api_keys::authenticate(&state.pool, token).await.map_err(ApiError::from)
        };
        match user {
            Ok(Some(user)) => Ok(ApiUser(user)),
            Ok(None) => {
                Err(unauthorized(state, &path, "This API key or access token is invalid, expired or has been revoked."))
            }
            Err(error) => Err(error.into_response()),
        }
    }
}

// ---------------------------------------------------------------------------
// Endpoints shared by every project
// ---------------------------------------------------------------------------

/// What this service is.
#[derive(Serialize, ToSchema)]
struct Info {
    name: &'static str,
    version: &'static str,
    /// The deployed build, `development` locally.
    revision: &'static str,
    /// The API guide, when the docs feature is on.
    docs: Option<&'static str>,
    /// This API's OpenAPI document.
    openapi: String,
}

/// Service name, version and deployment revision, for clients checking connectivity. No key required.
#[utoipa::path(get, path = "/", tag = "service", operation_id = "get_info", responses((status = OK, description = "The service", body = Info)))]
async fn info() -> Response {
    json_body(
        StatusCode::OK,
        &Info {
            name: crate::views::SITE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            revision: app_core::http::BUILD_REVISION,
            docs: cfg!(feature = "docs").then_some("/docs/api"),
            openapi: format!("{BASE_PATH}{OPENAPI_PATH}"),
        },
    )
}

/// The account behind an API key.
#[derive(Serialize, ToSchema)]
struct Account {
    id: Uuid,
    #[schema(example = "you@example.com")]
    email: String,
    /// The profile name; empty until set in Settings.
    name: String,
    is_admin: bool,
    #[schema(format = DateTime)]
    created_at: String,
}

/// The account behind the key.
#[utoipa::path(
    get, path = "/me", tag = "account", operation_id = "get_account",
    security(("api_key" = [])),
    responses(
        (status = OK, description = "The account", body = Account),
        (status = UNAUTHORIZED, description = "Missing, invalid or revoked key", body = ErrorBody),
    ),
)]
async fn me(State(state): State<SharedState>, ApiUser(user): ApiUser) -> Result<Response, ApiError> {
    Ok(json_response(StatusCode::OK, account_json(&state, &user).await?))
}

/// The `GET /api/v1/me` document; the MCP server's `get_account` tool returns the same.
pub async fn account_json(state: &SharedState, user: &User) -> Result<Value, ApiError> {
    let profile = crate::settings::profile(&state.pool, user.id).await?;
    let account = Account {
        id: user.id,
        email: profile.email,
        name: profile.name,
        is_admin: user.is_admin,
        created_at: user.created_at.format(&Rfc3339).unwrap_or_default(),
    };
    Ok(serde_json::to_value(account).expect("accounts serialize"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_documents_every_endpoint_and_its_authentication() {
        let document: Value = serde_json::from_str(openapi()).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["info"]["title"], format!("{} API", crate::views::SITE_NAME));
        let mut operations = Vec::new();
        for (path, item) in document["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                operations.push(format!("{} {path}", method.to_uppercase()));
                if path != BASE_PATH {
                    assert_eq!(operation["security"], json!([{ BEARER: [] }]), "{method} {path} needs a key");
                }
                assert!(operation["summary"].is_string(), "{method} {path} has a doc comment");
                for response in operation["responses"].as_object().unwrap().values() {
                    assert!(response["description"].is_string(), "{method} {path}: responses need a description");
                }
            }
        }
        operations.sort();
        // Add your endpoints here when you register them in `documented_routes`.
        assert_eq!(operations, ["GET /api/v1", "GET /api/v1/me"]);
        assert_eq!(document["components"]["securitySchemes"][BEARER]["scheme"], "bearer");
        let schemas = &document["components"]["schemas"];
        let mut account = schemas["Account"]["properties"].as_object().unwrap().keys().collect::<Vec<_>>();
        account.sort();
        assert_eq!(account, ["created_at", "email", "id", "is_admin", "name"]);
    }
}
