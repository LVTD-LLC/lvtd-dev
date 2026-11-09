//! OAuth 2.1 for the JSON API and the MCP server, mounted with the `api`
//! feature: hosted agents (Claude, ChatGPT, Claude Code and any MCP client
//! that follows the protocol's authorization specification) discover this
//! server from a `401`, register themselves, send the user here to sign in
//! and approve, and then call the API with a token they refresh on their
//! own. API keys keep working alongside for clients that paste a header.
//!
//! The protocol, the SQL and the token lookup are `app_auth::oauth_server`; this
//! module is the HTTP surface: the two metadata documents, registration,
//! the token and revocation endpoints (mounted outside the same-origin
//! guard, since clients call them from their own servers), the consent
//! page at `/oauth/authorize`, and the return trip to it after sign-in.
//! Connected apps are listed and disconnected on the settings page.

use crate::views::{self, Page};
use crate::{AppError, SharedState};
use app_auth::oauth_server::{self, AuthorizeError, AuthorizeRequest, Server};
use app_auth::{CurrentUser, MaybeUser};
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

/// Resource paths tokens can be requested for (RFC 8707): the MCP server
/// first, since that is the one MCP clients name.
const RESOURCES: &[&str] = if cfg!(feature = "mcp") { &["/mcp", "/api/v1"] } else { &["/api/v1"] };

/// What the metadata documents say; built once into `AppState`.
pub fn server(site_url: &str) -> Server {
    let guide = if cfg!(feature = "mcp") { "/docs/mcp" } else { "/docs/api" };
    Server::new(site_url, views::SITE_NAME, RESOURCES, cfg!(feature = "docs").then_some(guide))
}

/// `GET /.well-known/oauth-authorization-server` (RFC 8414).
pub async fn server_metadata(State(state): State<SharedState>) -> Response {
    Server::metadata_response(&state.oauth_server.metadata())
}

/// `GET /.well-known/oauth-protected-resource` (RFC 9728): the origin as a resource.
pub async fn resource_metadata(State(state): State<SharedState>) -> Response {
    Server::metadata_response(&state.oauth_server.resource_metadata(None))
}

/// `GET /.well-known/oauth-protected-resource/mcp`: what a `401` from `/mcp` points at.
#[cfg(feature = "mcp")]
pub async fn mcp_resource_metadata(State(state): State<SharedState>) -> Response {
    Server::metadata_response(&state.oauth_server.resource_metadata(Some(crate::mcp::PATH)))
}

/// `POST /oauth/register` (RFC 7591).
pub async fn register(State(state): State<SharedState>, body: Bytes) -> Response {
    oauth_server::register(&state.pool, &body).await
}

/// `POST /oauth/token`.
pub async fn token(State(state): State<SharedState>, headers: HeaderMap, body: Bytes) -> Response {
    oauth_server::token(&state.pool, &state.oauth_server, &headers, &body).await
}

/// `POST /oauth/revoke` (RFC 7009).
pub async fn revoke(State(state): State<SharedState>, headers: HeaderMap, body: Bytes) -> Response {
    oauth_server::revoke(&state.pool, &headers, &body).await
}

/// A refused request never redirects: the browser stays here with an explanation.
fn refused(state: &SharedState, user: Option<&app_auth::User>, message: &str) -> Response {
    let page = Page { title: "Authorization refused", path: "", user, body: views::oauth_server::refused(message) };
    crate::routes::html(StatusCode::BAD_REQUEST, views::layout(state, page))
}

/// `GET /oauth/authorize`: check the request, then ask the signed-in user.
/// Anonymous visitors sign in (or up) first and come back through
/// [`resume`] with the request remembered in a cookie.
pub async fn authorize_page(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    RawQuery(query): RawQuery,
) -> Result<Response, AppError> {
    let request = AuthorizeRequest::from_query(query.as_deref().unwrap_or_default());
    let authorization = match oauth_server::validate(&state.pool, &state.oauth_server, &request).await {
        Ok(authorization) => authorization,
        Err(AuthorizeError::Untrusted(message)) => return Ok(refused(&state, user.as_ref(), message)),
        Err(AuthorizeError::Redirect { url, .. }) => return Ok(Redirect::to(&url).into_response()),
        Err(AuthorizeError::Other(error)) => return Err(error.into()),
    };
    let secure = state.cfg.secure_cookies();
    let Some(user) = user else {
        return Ok(([(header::SET_COOKIE, oauth_server::continue_cookie(&request, secure))], Redirect::to("/sign-in"))
            .into_response());
    };
    let page = Page {
        title: "Connect an application",
        path: "",
        user: Some(&user),
        body: views::oauth_server::consent(&authorization, &user),
    };
    let mut response = crate::routes::html(StatusCode::OK, views::layout(&state, page));
    response.headers_mut().insert(header::SET_COOKIE, oauth_server::clear_continue_cookie(secure));
    Ok(response)
}

/// `POST /oauth/authorize`: the consent form's decision, carrying the
/// request as hidden fields. Checked again before a code is issued.
pub async fn authorize_decision(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    body: Bytes,
) -> Result<Response, AppError> {
    let pairs = oauth_server::form_pairs(&body);
    let request = AuthorizeRequest::from_pairs(&pairs);
    let authorization = match oauth_server::validate(&state.pool, &state.oauth_server, &request).await {
        Ok(authorization) => authorization,
        Err(AuthorizeError::Untrusted(message)) => return Ok(refused(&state, Some(&user), message)),
        Err(AuthorizeError::Redirect { url, .. }) => return Ok(Redirect::to(&url).into_response()),
        Err(AuthorizeError::Other(error)) => return Err(error.into()),
    };
    let allowed = pairs.iter().any(|(name, value)| name == "decision" && value == "allow");
    let url = if allowed {
        let url = authorization.code_url(&state.pool, &state.oauth_server, user.id).await?;
        #[cfg(feature = "posthog")]
        state.track(&user, "oauth app connected", serde_json::json!({ "client_name": authorization.client.name }));
        url
    } else {
        authorization.denied_url(&state.oauth_server)
    };
    Ok(Redirect::to(&url).into_response())
}

/// Where every sign-in path lands checks for a remembered authorization
/// request and sends the browser back to it, once.
pub fn resume(headers: &HeaderMap, secure: bool) -> Option<Response> {
    let path = oauth_server::continue_path(headers)?;
    Some(([(header::SET_COOKIE, oauth_server::clear_continue_cookie(secure))], Redirect::to(&path)).into_response())
}
