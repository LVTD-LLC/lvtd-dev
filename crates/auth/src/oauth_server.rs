//! An OAuth 2.1 authorization server for the JSON API and the MCP server,
//! so hosted agents (Claude, ChatGPT and any MCP client that follows the
//! protocol's authorization specification) can connect an account with a
//! sign-in and a consent screen instead of a pasted API key.
//!
//! What it speaks: authorization server metadata (RFC 8414) at
//! [`SERVER_METADATA_PATH`], protected resource metadata (RFC 9728) at
//! [`RESOURCE_METADATA_PATH`], dynamic client registration (RFC 7591) at
//! [`REGISTER_PATH`], the authorization code grant with PKCE `S256`
//! required for every client (OAuth 2.1), refresh tokens that rotate on
//! use, resource indicators (RFC 8707), token revocation (RFC 7009) and
//! the `iss` response parameter (RFC 9207). Access tokens are opaque
//! `sro_` strings looked up by hash, exactly like API keys, so a leaked
//! database leaks nothing usable; there are no JWTs and no signing keys.
//!
//! Division of labour: this module owns the protocol and the SQL
//! (`oauth_clients`, `oauth_codes`, `oauth_tokens`). The app owns the HTML:
//! it renders the consent page from an [`Authorization`], posts the user's
//! decision back through [`Authorization::code_url`] or [`error_url`], and
//! mounts the JSON endpoints ([`register`], [`token`], [`revoke`], the two
//! metadata documents) outside its same-origin guard, because clients call
//! them from their own servers. Bearer lookup is [`authenticate`]; the
//! app's API extractor calls it for tokens with the [`ACCESS_PREFIX`].
//!
//! A token has the same access as the account, like an API key: there is
//! no scope model. The `scope` a client asks for is stored and echoed so
//! clients that insist on one keep working.

use crate::users::User;
use app_core::Pool;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use url::Url;
use uuid::Uuid;

pub const SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
pub const RESOURCE_METADATA_PATH: &str = "/.well-known/oauth-protected-resource";
pub const AUTHORIZE_PATH: &str = "/oauth/authorize";
pub const TOKEN_PATH: &str = "/oauth/token";
pub const REGISTER_PATH: &str = "/oauth/register";
pub const REVOKE_PATH: &str = "/oauth/revoke";

/// Access tokens start with this, so the API extractor can route them here
/// without a database round trip for API keys.
pub const ACCESS_PREFIX: &str = "sro_";
pub const REFRESH_PREFIX: &str = "srr_";
/// How long a client has to exchange an authorization code.
pub const CODE_TTL: Duration = Duration::minutes(10);
pub const ACCESS_TTL: Duration = Duration::hours(1);
/// A refresh token lives this long; using it issues a fresh pair, so an
/// agent in regular use never needs a second consent.
pub const REFRESH_TTL: Duration = Duration::days(30);
/// Live grants per account. Older ones are dropped first.
pub const MAX_TOKENS_PER_USER: i64 = 25;
/// Registered clients that never issued a token are removed after this.
const UNUSED_CLIENT_DAYS: f64 = 90.0;
const MAX_REDIRECT_URIS: usize = 10;
const CLIENT_NAME_MAX: usize = 100;
const DEFAULT_CLIENT_NAME: &str = "An application";
const AUTH_METHODS: [&str; 3] = ["none", "client_secret_basic", "client_secret_post"];
const GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];

/// What this server tells clients about itself. Built once from the
/// configuration and kept in the app state.
#[derive(Clone, Debug)]
pub struct Server {
    /// The site origin, e.g. `https://example.com`.
    pub issuer: String,
    /// Shown as `resource_name` in the resource metadata.
    pub name: String,
    /// Absolute resource identifiers a client may request a token for
    /// (RFC 8707). The first is the one advertised to MCP clients.
    pub resources: Vec<String>,
    /// A page explaining how to connect, linked from both metadata documents.
    pub documentation: Option<String>,
}

impl Server {
    /// `resource_paths` are paths on the issuer, e.g. `/mcp` and `/api/v1`.
    pub fn new(site_url: &str, name: &str, resource_paths: &[&str], documentation: Option<&str>) -> Self {
        let issuer = site_url.trim_end_matches('/').to_owned();
        Self {
            resources: resource_paths.iter().map(|path| format!("{issuer}{path}")).collect(),
            documentation: documentation.map(|path| format!("{issuer}{path}")),
            issuer,
            name: name.to_owned(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.issuer)
    }

    /// Where the protected resource metadata for `resource_path` lives
    /// (`/.well-known/oauth-protected-resource/mcp` for `/mcp`); the root
    /// document describes the origin itself.
    pub fn resource_metadata_path(&self, resource_path: &str) -> String {
        format!("{RESOURCE_METADATA_PATH}{}", resource_path.trim_end_matches('/'))
    }

    /// The `WWW-Authenticate` value for a 401 on `resource_path`, which is
    /// how an MCP client finds this authorization server.
    pub fn www_authenticate(&self, realm: &str, resource_path: &str) -> HeaderValue {
        let url = self.url(&self.resource_metadata_path(resource_path));
        HeaderValue::from_str(&format!("Bearer realm=\"{realm}\", resource_metadata=\"{url}\""))
            .expect("the issuer is a valid header value")
    }

    /// The RFC 8414 document.
    pub fn metadata(&self) -> Value {
        let mut document = json!({
            "issuer": self.issuer,
            "authorization_endpoint": self.url(AUTHORIZE_PATH),
            "token_endpoint": self.url(TOKEN_PATH),
            "registration_endpoint": self.url(REGISTER_PATH),
            "revocation_endpoint": self.url(REVOKE_PATH),
            "response_types_supported": ["code"],
            "response_modes_supported": ["query"],
            "grant_types_supported": GRANT_TYPES,
            "token_endpoint_auth_methods_supported": AUTH_METHODS,
            "revocation_endpoint_auth_methods_supported": AUTH_METHODS,
            "code_challenge_methods_supported": ["S256"],
            "authorization_response_iss_parameter_supported": true,
        });
        if let Some(documentation) = &self.documentation {
            document["service_documentation"] = Value::String(documentation.clone());
        }
        document
    }

    /// The RFC 9728 document for one resource (`None` for the origin itself).
    pub fn resource_metadata(&self, resource_path: Option<&str>) -> Value {
        let mut document = json!({
            "resource": match resource_path { Some(path) => self.url(path), None => self.issuer.clone() },
            "authorization_servers": [self.issuer],
            "bearer_methods_supported": ["header"],
            "resource_name": self.name,
        });
        if let Some(documentation) = &self.documentation {
            document["resource_documentation"] = Value::String(documentation.clone());
        }
        document
    }

    /// A metadata document as a response: public and briefly cacheable.
    pub fn metadata_response(document: &Value) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")],
            document.to_string(),
        )
            .into_response()
    }

    fn accepts_resource(&self, resource: &str) -> bool {
        self.resources.iter().any(|known| known == resource)
    }
}

// ---------------------------------------------------------------------------
// Tokens and hashing
// ---------------------------------------------------------------------------

fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn random(prefix: &str) -> anyhow::Result<String> {
    Ok(format!("{prefix}{}", crate::random_hex(32)?))
}

fn no_store(status: StatusCode, body: Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        body.to_string(),
    )
        .into_response()
}

/// An RFC 6749 error response; `invalid_client` carries the challenge the
/// client should have met.
fn error_response(status: StatusCode, error: &str, description: &str) -> Response {
    let mut response = no_store(status, json!({ "error": error, "error_description": description }));
    if error == "invalid_client" {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"oauth\""));
    }
    response
}

fn internal(error: anyhow::Error) -> Response {
    tracing::error!(error = ?error, "oauth request failed");
    let mut response = no_store(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "server_error" }));
    response.extensions_mut().insert(std::sync::Arc::new(error));
    response
}

/// `application/x-www-form-urlencoded` pairs, as the token and consent
/// endpoints receive them.
pub fn form_pairs(body: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(body).into_owned().collect()
}

fn first<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn printable(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}

// ---------------------------------------------------------------------------
// Redirect URIs and PKCE (pure)
// ---------------------------------------------------------------------------

fn is_loopback(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost") | Some("127.0.0.1") | Some("[::1]"))
}

/// A redirect URI a client may register: absolute, without a fragment,
/// `https` for the web, `http` only on the loopback interface (RFC 8252
/// §8.3) and private-use schemes for native apps (§7.1).
pub fn valid_redirect_uri(uri: &str) -> bool {
    if !printable(uri, 2048) {
        return false;
    }
    let Ok(url) = Url::parse(uri) else { return false };
    if url.fragment().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => is_loopback(&url),
        "javascript" | "data" | "file" | "blob" | "about" | "vbscript" => false,
        _ => true,
    }
}

/// Exact match, except that a loopback `http` URI may use any port
/// (RFC 8252 §7.3): the client only learns its port when it starts.
pub fn redirect_uri_matches(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    let (Ok(a), Ok(b)) = (Url::parse(registered), Url::parse(requested)) else { return false };
    a.scheme() == "http"
        && b.scheme() == "http"
        && is_loopback(&a)
        && is_loopback(&b)
        && a.host_str() == b.host_str()
        && a.path() == b.path()
        && a.query() == b.query()
}

fn valid_code_challenge(challenge: &str) -> bool {
    (43..=128).contains(&challenge.len())
        && challenge.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `S256`: the challenge is the base64url (no padding) SHA-256 of the verifier.
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    valid_code_challenge(verifier) && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

/// Append query parameters to a redirect URI, keeping any it already has.
fn with_query(redirect_uri: &str, pairs: &[(&str, &str)]) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        query.append_pair(name, value);
    }
    let query = query.finish();
    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    format!("{redirect_uri}{separator}{query}")
}

/// Where to send the browser when an authorization request fails after its
/// client and redirect URI were verified (RFC 6749 §4.1.2.1).
pub fn error_url(server: &Server, redirect_uri: &str, state: Option<&str>, error: &str, description: &str) -> String {
    let mut pairs = vec![("error", error), ("error_description", description), ("iss", server.issuer.as_str())];
    if let Some(state) = state {
        pairs.push(("state", state));
    }
    with_query(redirect_uri, &pairs)
}

// ---------------------------------------------------------------------------
// Clients: dynamic registration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub client_uri: Option<String>,
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: String,
    pub grant_types: Vec<String>,
    secret_hash: Option<Vec<u8>>,
}

impl Client {
    /// The host of `client_uri`, for consent pages to show who is asking.
    pub fn origin(&self) -> Option<String> {
        self.client_uri.as_deref().and_then(|uri| Url::parse(uri).ok()?.host_str().map(str::to_owned))
    }

    fn is_public(&self) -> bool {
        self.token_endpoint_auth_method == "none"
    }

    fn may_refresh(&self) -> bool {
        self.grant_types.iter().any(|grant| grant == "refresh_token")
    }
}

#[derive(Default, Deserialize)]
struct RegistrationRequest {
    #[serde(default)]
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    client_uri: Option<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    scope: Option<String>,
}

async fn find_client(pool: &Pool, id: &str) -> anyhow::Result<Option<Client>> {
    if !printable(id, 128) {
        return Ok(None);
    }
    Ok(sqlx::query_as!(
        Client,
        "SELECT id, name, client_uri, redirect_uris, token_endpoint_auth_method, grant_types, secret_hash \
         FROM oauth_clients WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await?)
}

/// `POST /oauth/register` (RFC 7591). Anyone may register: a client is
/// just a name and its redirect URIs, and nothing happens until a user
/// consents. Unused registrations are purged after [`UNUSED_CLIENT_DAYS`].
#[tracing::instrument(name = "oauth.register", skip_all)]
pub async fn register(pool: &Pool, body: &[u8]) -> Response {
    let request: RegistrationRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                &format!("The registration body must be a JSON object: {error}."),
            );
        }
    };
    match register_client(pool, request).await {
        Ok(document) => no_store(StatusCode::CREATED, document),
        Err(RegistrationError::Invalid(error, description)) => {
            error_response(StatusCode::BAD_REQUEST, error, &description)
        }
        Err(RegistrationError::Other(error)) => internal(error),
    }
}

enum RegistrationError {
    Invalid(&'static str, String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RegistrationError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<sqlx::Error> for RegistrationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

async fn register_client(pool: &Pool, request: RegistrationRequest) -> Result<Value, RegistrationError> {
    use RegistrationError::Invalid;
    if request.redirect_uris.is_empty() || request.redirect_uris.len() > MAX_REDIRECT_URIS {
        return Err(Invalid(
            "invalid_redirect_uri",
            format!("Register between 1 and {MAX_REDIRECT_URIS} redirect_uris."),
        ));
    }
    if let Some(bad) = request.redirect_uris.iter().find(|uri| !valid_redirect_uri(uri)) {
        return Err(Invalid(
            "invalid_redirect_uri",
            format!(
                "{bad:?} is not an acceptable redirect URI: use https, http on localhost, or a private-use scheme, without a fragment."
            ),
        ));
    }
    let name = match request.client_name.as_deref().map(str::trim) {
        None | Some("") => DEFAULT_CLIENT_NAME.to_owned(),
        Some(name) if name.chars().count() <= CLIENT_NAME_MAX && !name.chars().any(char::is_control) => name.to_owned(),
        Some(_) => {
            return Err(Invalid(
                "invalid_client_metadata",
                format!("client_name must be at most {CLIENT_NAME_MAX} characters without control characters."),
            ));
        }
    };
    let client_uri = match request.client_uri.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(uri) if printable(uri, 2048) && Url::parse(uri).is_ok_and(|url| url.scheme() == "https") => {
            Some(uri.to_owned())
        }
        Some(_) => return Err(Invalid("invalid_client_metadata", "client_uri must be an https URL.".into())),
    };
    let auth_method = request.token_endpoint_auth_method.unwrap_or_else(|| "client_secret_basic".into());
    if !AUTH_METHODS.contains(&auth_method.as_str()) {
        return Err(Invalid(
            "invalid_client_metadata",
            format!("token_endpoint_auth_method must be one of {}.", AUTH_METHODS.join(", ")),
        ));
    }
    let grant_types = request.grant_types.unwrap_or_else(|| vec!["authorization_code".into()]);
    if grant_types.is_empty() || grant_types.iter().any(|grant| !GRANT_TYPES.contains(&grant.as_str())) {
        return Err(Invalid(
            "invalid_client_metadata",
            format!("grant_types may only contain {}.", GRANT_TYPES.join(" and ")),
        ));
    }
    if !grant_types.iter().any(|grant| grant == "authorization_code") {
        return Err(Invalid("invalid_client_metadata", "grant_types must include authorization_code.".into()));
    }
    if let Some(types) = &request.response_types
        && types.iter().any(|kind| kind != "code")
    {
        return Err(Invalid("invalid_client_metadata", "Only the code response type is supported.".into()));
    }
    let scope = match request.scope.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(scope) if printable(scope, 256) => Some(scope.to_owned()),
        Some(_) => return Err(Invalid("invalid_client_metadata", "scope is too long.".into())),
    };
    let id = crate::random_hex(16)?;
    let secret = if auth_method == "none" { None } else { Some(random("")?) };
    let row = sqlx::query!(
        "INSERT INTO oauth_clients (id, secret_hash, name, client_uri, redirect_uris, token_endpoint_auth_method, grant_types, scope) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING created_at",
        id,
        secret.as_deref().map(hash),
        name,
        client_uri,
        &request.redirect_uris,
        auth_method,
        &grant_types,
        scope
    )
    .fetch_one(pool)
    .await?;
    let mut document = json!({
        "client_id": id,
        "client_id_issued_at": row.created_at.unix_timestamp(),
        "client_name": name,
        "redirect_uris": request.redirect_uris,
        "token_endpoint_auth_method": auth_method,
        "grant_types": grant_types,
        "response_types": ["code"],
    });
    if let Some(secret) = secret {
        document["client_secret"] = Value::String(secret);
        document["client_secret_expires_at"] = json!(0);
    }
    if let Some(uri) = client_uri {
        document["client_uri"] = Value::String(uri);
    }
    if let Some(scope) = scope {
        document["scope"] = Value::String(scope);
    }
    Ok(document)
}

// ---------------------------------------------------------------------------
// Authorization requests and codes
// ---------------------------------------------------------------------------

/// The parameters of `GET /oauth/authorize`, also replayed as hidden
/// fields by the consent form so the decision carries the same request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthorizeRequest {
    pub response_type: Option<String>,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub state: Option<String>,
    pub scope: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub resource: Option<String>,
}

impl AuthorizeRequest {
    pub fn from_query(query: &str) -> Self {
        Self::from_pairs(&form_pairs(query.as_bytes()))
    }

    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let field = |name: &str| first(pairs, name).map(str::to_owned);
        Self {
            response_type: field("response_type"),
            client_id: field("client_id"),
            redirect_uri: field("redirect_uri"),
            state: field("state"),
            scope: field("scope"),
            code_challenge: field("code_challenge"),
            code_challenge_method: field("code_challenge_method"),
            resource: field("resource"),
        }
    }

    /// The fields the consent form must post back, in order.
    pub fn fields(&self) -> Vec<(&'static str, &str)> {
        [
            ("response_type", &self.response_type),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("state", &self.state),
            ("scope", &self.scope),
            ("code_challenge", &self.code_challenge),
            ("code_challenge_method", &self.code_challenge_method),
            ("resource", &self.resource),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.as_deref().map(|value| (name, value)))
        .collect()
    }

    /// The query string that reproduces this request, for the sign-in return trip.
    pub fn to_query(&self) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (name, value) in self.fields() {
            query.append_pair(name, value);
        }
        query.finish()
    }
}

/// A verified authorization request: the client exists, the redirect URI
/// is one of its own, and every other parameter is acceptable. What the
/// consent page shows and what a decision acts on.
#[derive(Clone, Debug)]
pub struct Authorization {
    pub client: Client,
    pub request: AuthorizeRequest,
    pub redirect_uri: String,
    pub scope: String,
    pub resource: Option<String>,
    code_challenge: String,
}

#[derive(Debug)]
pub enum AuthorizeError {
    /// The client or its redirect URI could not be trusted, so the browser
    /// must not be redirected anywhere: show the message instead.
    Untrusted(&'static str),
    /// A problem the client should hear about at its redirect URI.
    Redirect {
        url: String,
        error: &'static str,
    },
    Other(anyhow::Error),
}

impl From<anyhow::Error> for AuthorizeError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<sqlx::Error> for AuthorizeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

/// Check a request before showing consent and again before issuing a code.
#[tracing::instrument(name = "oauth.validate", skip_all)]
pub async fn validate(
    pool: &Pool,
    server: &Server,
    request: &AuthorizeRequest,
) -> Result<Authorization, AuthorizeError> {
    use AuthorizeError::Untrusted;
    let client_id = request.client_id.as_deref().filter(|id| !id.is_empty()).ok_or(Untrusted(
        "The authorization request names no client_id. The application that sent you here must register first.",
    ))?;
    let client = find_client(pool, client_id).await?.ok_or(Untrusted(
        "Unknown client_id. The application that sent you here is not registered with this server; it may need to reconnect.",
    ))?;
    let redirect_uri = match request.redirect_uri.as_deref() {
        Some(uri) => client
            .redirect_uris
            .iter()
            .find(|registered| redirect_uri_matches(registered, uri))
            .map(|_| uri.to_owned())
            .ok_or(Untrusted("The redirect_uri is not one this application registered, so the request was refused."))?,
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => return Err(Untrusted("The authorization request must name a redirect_uri.")),
    };
    if let Some(state) = &request.state
        && !printable(state, 2048)
    {
        return Err(Untrusted("The state parameter is too long."));
    }
    let state = request.state.as_deref();
    let redirect = |error: &'static str, description: &str| AuthorizeError::Redirect {
        url: error_url(server, &redirect_uri, state, error, description),
        error,
    };
    if request.response_type.as_deref() != Some("code") {
        return Err(redirect("unsupported_response_type", "Only response_type=code is supported."));
    }
    let code_challenge = match request.code_challenge.as_deref() {
        Some(challenge) if valid_code_challenge(challenge) => challenge.to_owned(),
        Some(_) => return Err(redirect("invalid_request", "code_challenge must be 43 to 128 unreserved characters.")),
        None => return Err(redirect("invalid_request", "PKCE is required: send code_challenge with method S256.")),
    };
    if request.code_challenge_method.as_deref() != Some("S256") {
        return Err(redirect("invalid_request", "Only code_challenge_method=S256 is supported."));
    }
    let scope = request.scope.clone().unwrap_or_default();
    if !printable(&scope, 256) {
        return Err(redirect("invalid_scope", "The requested scope is too long."));
    }
    let resource = match request.resource.as_deref().filter(|r| !r.is_empty()) {
        None => None,
        Some(resource) if server.accepts_resource(resource) => Some(resource.to_owned()),
        Some(_) => {
            return Err(redirect(
                "invalid_target",
                &format!("Unknown resource. This server issues tokens for: {}.", server.resources.join(", ")),
            ));
        }
    };
    Ok(Authorization { client, request: request.clone(), redirect_uri, scope, resource, code_challenge })
}

impl Authorization {
    /// The user said yes: store a code bound to them and return where to
    /// send the browser (the code, the client's `state` and `iss`).
    #[tracing::instrument(name = "oauth.issue_code", skip_all)]
    pub async fn code_url(&self, pool: &Pool, server: &Server, user_id: Uuid) -> anyhow::Result<String> {
        let code = random("")?;
        let mut tx = pool.begin().await?;
        sqlx::query!(
            "INSERT INTO oauth_codes (id, code_hash, client_id, user_id, redirect_uri, scope, resource, code_challenge, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            Uuid::now_v7(),
            hash(&code),
            self.client.id,
            user_id,
            self.redirect_uri,
            self.scope,
            self.resource,
            self.code_challenge,
            OffsetDateTime::now_utc() + CODE_TTL
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE oauth_clients SET last_used_at = now() WHERE id = $1 \
             AND (last_used_at IS NULL OR last_used_at < now() - interval '1 hour')",
            self.client.id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut pairs = vec![("code", code.as_str()), ("iss", server.issuer.as_str())];
        if let Some(state) = &self.request.state {
            pairs.push(("state", state));
        }
        Ok(with_query(&self.redirect_uri, &pairs))
    }

    /// The user said no.
    pub fn denied_url(&self, server: &Server) -> String {
        error_url(server, &self.redirect_uri, self.request.state.as_deref(), "access_denied", "The user declined.")
    }
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

struct TokenError(StatusCode, &'static str, String);

impl TokenError {
    fn invalid(error: &'static str, description: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, error, description.into())
    }

    fn client(description: impl Into<String>) -> Self {
        Self(StatusCode::UNAUTHORIZED, "invalid_client", description.into())
    }
}

impl From<anyhow::Error> for TokenError {
    fn from(error: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "server_error", error.to_string())
    }
}

impl From<sqlx::Error> for TokenError {
    fn from(error: sqlx::Error) -> Self {
        anyhow::Error::from(error).into()
    }
}

/// Which client is calling, from `Authorization: Basic` (RFC 6749 §2.3.1)
/// or the body. Public clients send only `client_id`.
async fn authenticate_client(
    pool: &Pool,
    headers: &HeaderMap,
    pairs: &[(String, String)],
) -> Result<Client, TokenError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .map(|(_, credentials)| credentials.trim());
    let (id, secret) = match basic {
        Some(credentials) => {
            let decoded = STANDARD
                .decode(credentials)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .ok_or_else(|| TokenError::client("The Basic credentials are not base64."))?;
            let (id, secret) = decoded
                .split_once(':')
                .ok_or_else(|| TokenError::client("Basic credentials need an id and a secret."))?;
            let decode = |part: &str| form_urlencoded::parse(part.as_bytes()).next().map(|(k, _)| k.into_owned());
            (decode(id).unwrap_or_default(), decode(secret).filter(|s| !s.is_empty()))
        }
        None => (
            first(pairs, "client_id").unwrap_or_default().to_owned(),
            first(pairs, "client_secret").filter(|s| !s.is_empty()).map(str::to_owned),
        ),
    };
    if id.is_empty() {
        return Err(TokenError::client("Identify the client with client_id."));
    }
    let client = find_client(pool, &id).await?.ok_or_else(|| TokenError::client("Unknown client_id."))?;
    if client.is_public() {
        return Ok(client);
    }
    match (&client.secret_hash, secret) {
        (Some(expected), Some(secret)) if *expected == hash(&secret) => Ok(client),
        _ => Err(TokenError::client("The client secret is missing or wrong.")),
    }
}

struct TokenSet {
    access: String,
    refresh: Option<String>,
    scope: String,
}

impl TokenSet {
    fn document(&self) -> Value {
        let mut document = json!({
            "access_token": self.access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL.whole_seconds(),
        });
        if let Some(refresh) = &self.refresh {
            document["refresh_token"] = Value::String(refresh.clone());
        }
        if !self.scope.is_empty() {
            document["scope"] = Value::String(self.scope.clone());
        }
        document
    }
}

async fn issue(
    tx: &mut sqlx::PgConnection,
    client: &Client,
    user_id: Uuid,
    scope: &str,
    resource: Option<&str>,
) -> anyhow::Result<TokenSet> {
    let access = random(ACCESS_PREFIX)?;
    let refresh = if client.may_refresh() { Some(random(REFRESH_PREFIX)?) } else { None };
    let now = OffsetDateTime::now_utc();
    sqlx::query!(
        "INSERT INTO oauth_tokens (id, client_id, user_id, access_hash, refresh_hash, scope, resource, access_expires_at, refresh_expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        Uuid::now_v7(),
        client.id,
        user_id,
        hash(&access),
        refresh.as_deref().map(hash),
        scope,
        resource,
        now + ACCESS_TTL,
        refresh.as_ref().map(|_| now + REFRESH_TTL)
    )
    .execute(&mut *tx)
    .await?;
    // Keep the account's grant list bounded; the oldest connections go first.
    sqlx::query!(
        "DELETE FROM oauth_tokens WHERE user_id = $1 AND id NOT IN \
         (SELECT id FROM oauth_tokens WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2)",
        user_id,
        MAX_TOKENS_PER_USER
    )
    .execute(&mut *tx)
    .await?;
    Ok(TokenSet { access, refresh, scope: scope.to_owned() })
}

/// `POST /oauth/token`: `authorization_code` with PKCE, or `refresh_token`,
/// which rotates the pair. Mount it outside the same-origin guard.
#[tracing::instrument(name = "oauth.token", skip_all)]
pub async fn token(pool: &Pool, server: &Server, headers: &HeaderMap, body: &[u8]) -> Response {
    let pairs = form_pairs(body);
    match grant(pool, server, headers, &pairs).await {
        Ok(tokens) => no_store(StatusCode::OK, tokens.document()),
        Err(TokenError(StatusCode::INTERNAL_SERVER_ERROR, _, description)) => internal(anyhow::anyhow!(description)),
        Err(TokenError(status, error, description)) => error_response(status, error, &description),
    }
}

async fn grant(
    pool: &Pool,
    server: &Server,
    headers: &HeaderMap,
    pairs: &[(String, String)],
) -> Result<TokenSet, TokenError> {
    let client = authenticate_client(pool, headers, pairs).await?;
    match first(pairs, "grant_type") {
        Some("authorization_code") => exchange_code(pool, server, &client, pairs).await,
        Some("refresh_token") => refresh(pool, &client, pairs).await,
        Some(other) => Err(TokenError::invalid("unsupported_grant_type", format!("Unsupported grant_type {other:?}."))),
        None => Err(TokenError::invalid("invalid_request", "grant_type is required.")),
    }
}

async fn exchange_code(
    pool: &Pool,
    server: &Server,
    client: &Client,
    pairs: &[(String, String)],
) -> Result<TokenSet, TokenError> {
    let code = first(pairs, "code")
        .filter(|c| printable(c, 256))
        .ok_or_else(|| TokenError::invalid("invalid_request", "code is required."))?;
    let verifier = first(pairs, "code_verifier")
        .ok_or_else(|| TokenError::invalid("invalid_request", "code_verifier is required (PKCE)."))?;
    // Single use: the row goes away whatever happens next, so a verifier
    // cannot be guessed against the same code.
    let stored = sqlx::query!(
        "DELETE FROM oauth_codes WHERE code_hash = $1 \
         RETURNING client_id, user_id, redirect_uri, scope, resource, code_challenge, expires_at",
        hash(code)
    )
    .fetch_optional(pool)
    .await?;
    let Some(stored) = stored else {
        return Err(TokenError::invalid("invalid_grant", "Unknown, used or expired authorization code."));
    };
    if stored.client_id != client.id {
        return Err(TokenError::invalid("invalid_grant", "This code was issued to another client."));
    }
    if stored.expires_at <= OffsetDateTime::now_utc() {
        return Err(TokenError::invalid("invalid_grant", "The authorization code expired; start over."));
    }
    if let Some(redirect_uri) = first(pairs, "redirect_uri")
        && redirect_uri != stored.redirect_uri
    {
        return Err(TokenError::invalid("invalid_grant", "redirect_uri does not match the authorization request."));
    }
    if !pkce_matches(verifier, &stored.code_challenge) {
        return Err(TokenError::invalid("invalid_grant", "code_verifier does not match the code_challenge."));
    }
    if let Some(resource) = first(pairs, "resource").filter(|r| !r.is_empty()) {
        if !server.accepts_resource(resource) {
            return Err(TokenError::invalid("invalid_target", "Unknown resource."));
        }
        if stored.resource.as_deref().is_some_and(|requested| requested != resource) {
            return Err(TokenError::invalid("invalid_target", "resource differs from the authorization request."));
        }
    }
    let mut tx = pool.begin().await?;
    let tokens = issue(&mut tx, client, stored.user_id, &stored.scope, stored.resource.as_deref()).await?;
    tx.commit().await?;
    Ok(tokens)
}

async fn refresh(pool: &Pool, client: &Client, pairs: &[(String, String)]) -> Result<TokenSet, TokenError> {
    let token = first(pairs, "refresh_token")
        .filter(|t| t.starts_with(REFRESH_PREFIX) && printable(t, 128))
        .ok_or_else(|| TokenError::invalid("invalid_grant", "A refresh_token from this server is required."))?;
    // Rotation: the old pair is gone before the new one exists.
    let stored = sqlx::query!(
        "DELETE FROM oauth_tokens WHERE refresh_hash = $1 AND client_id = $2 \
         RETURNING user_id, scope, resource, refresh_expires_at",
        hash(token),
        client.id
    )
    .fetch_optional(pool)
    .await?;
    let Some(stored) = stored else {
        return Err(TokenError::invalid("invalid_grant", "Unknown, revoked or already used refresh token."));
    };
    if stored.refresh_expires_at.is_none_or(|at| at <= OffsetDateTime::now_utc()) {
        return Err(TokenError::invalid("invalid_grant", "The refresh token expired; authorize again."));
    }
    let mut tx = pool.begin().await?;
    let tokens = issue(&mut tx, client, stored.user_id, &stored.scope, stored.resource.as_deref()).await?;
    tx.commit().await?;
    Ok(tokens)
}

/// `POST /oauth/revoke` (RFC 7009): drop the grant behind an access or
/// refresh token of the calling client. Always 200, as the RFC asks.
#[tracing::instrument(name = "oauth.revoke", skip_all)]
pub async fn revoke(pool: &Pool, headers: &HeaderMap, body: &[u8]) -> Response {
    let pairs = form_pairs(body);
    let client = match authenticate_client(pool, headers, &pairs).await {
        Ok(client) => client,
        Err(TokenError(StatusCode::INTERNAL_SERVER_ERROR, _, description)) => {
            return internal(anyhow::anyhow!(description));
        }
        Err(TokenError(status, error, description)) => return error_response(status, error, &description),
    };
    let Some(token) = first(&pairs, "token").filter(|t| printable(t, 128)) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", "token is required.");
    };
    let result = sqlx::query!(
        "DELETE FROM oauth_tokens WHERE client_id = $1 AND (access_hash = $2 OR refresh_hash = $2)",
        client.id,
        hash(token)
    )
    .execute(pool)
    .await;
    match result {
        Ok(_) => no_store(StatusCode::OK, json!({})),
        Err(error) => internal(error.into()),
    }
}

// ---------------------------------------------------------------------------
// Bearer lookup, grants and cleanup
// ---------------------------------------------------------------------------

/// Resolve an access token to its owner, or `None` if unknown, expired or
/// revoked. Records usage at most once a minute, like API keys.
#[tracing::instrument(name = "oauth.authenticate", skip_all)]
pub async fn authenticate(pool: &Pool, token: &str) -> anyhow::Result<Option<User>> {
    if !token.starts_with(ACCESS_PREFIX) || token.len() > 128 {
        return Ok(None);
    }
    let hash = hash(token);
    let row = sqlx::query!(
        r#"WITH touched AS (
               UPDATE oauth_tokens SET last_used_at = now()
               WHERE access_hash = $1 AND access_expires_at > now()
                 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')
           )
           SELECT u.id, u.email, u.created_at, u.is_admin, u.avatar_key
           FROM oauth_tokens t JOIN users u ON u.id = t.user_id
           WHERE t.access_hash = $1 AND t.access_expires_at > now()"#,
        hash
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| User {
        id: r.id,
        email: r.email,
        created_at: r.created_at,
        is_admin: r.is_admin,
        avatar_key: r.avatar_key,
    }))
}

/// A connection shown on the settings page: one issued token pair.
#[derive(Clone, Debug)]
pub struct Grant {
    pub id: Uuid,
    pub client_name: String,
    pub client_uri: Option<String>,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    /// When the connection lapses unless the client refreshes it.
    pub expires_at: OffsetDateTime,
}

#[tracing::instrument(name = "oauth.grants", skip_all)]
pub async fn grants_for_user(pool: &Pool, user_id: Uuid) -> anyhow::Result<Vec<Grant>> {
    Ok(sqlx::query_as!(
        Grant,
        r#"SELECT t.id, c.name AS client_name, c.client_uri, t.created_at, t.last_used_at,
                  COALESCE(t.refresh_expires_at, t.access_expires_at) AS "expires_at!"
           FROM oauth_tokens t JOIN oauth_clients c ON c.id = t.client_id
           WHERE t.user_id = $1 AND COALESCE(t.refresh_expires_at, t.access_expires_at) > now()
           ORDER BY t.created_at DESC"#,
        user_id
    )
    .fetch_all(pool)
    .await?)
}

/// Disconnect one grant. Someone else's is simply not found.
#[tracing::instrument(name = "oauth.revoke_grant", skip_all)]
pub async fn revoke_grant(pool: &Pool, id: Uuid, user_id: Uuid) -> anyhow::Result<bool> {
    let n = sqlx::query!("DELETE FROM oauth_tokens WHERE id = $1 AND user_id = $2", id, user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// Remove expired codes and tokens, and clients nobody has used for
/// [`UNUSED_CLIENT_DAYS`] that hold no token. Returns the row count.
pub async fn purge_expired(pool: &Pool) -> anyhow::Result<u64> {
    let codes = sqlx::query!("DELETE FROM oauth_codes WHERE expires_at <= now()").execute(pool).await?.rows_affected();
    let tokens =
        sqlx::query!("DELETE FROM oauth_tokens WHERE COALESCE(refresh_expires_at, access_expires_at) <= now()")
            .execute(pool)
            .await?
            .rows_affected();
    let clients = sqlx::query!(
        "DELETE FROM oauth_clients c WHERE COALESCE(c.last_used_at, c.created_at) < now() - make_interval(days => $1) \
         AND NOT EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.client_id = c.id) \
         AND NOT EXISTS (SELECT 1 FROM oauth_codes o WHERE o.client_id = c.id)",
        UNUSED_CLIENT_DAYS as i32
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(codes + tokens + clients)
}

/// Purge immediately, then hourly; spawn once per worker process like
/// `sessions::cleanup_expired`.
pub async fn cleanup_expired(pool: Pool) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match purge_expired(&pool).await {
            Ok(purged) if purged > 0 => tracing::info!(purged, "removed expired oauth rows"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to remove expired oauth rows; will retry next tick"),
        }
    }
}

// ---------------------------------------------------------------------------
// Returning to the authorization request after sign-in
// ---------------------------------------------------------------------------

/// Holds the authorization request while an anonymous visitor signs in or
/// signs up. The dashboard, where every sign-in path lands, sends them back.
pub const CONTINUE_COOKIE: &str = "oauth_continue";
const CONTINUE_TTL_SECONDS: u32 = 15 * 60;

fn cookie_flags(secure: bool) -> &'static str {
    if secure { "; Path=/; HttpOnly; SameSite=Lax; Secure" } else { "; Path=/; HttpOnly; SameSite=Lax" }
}

/// Remember `request` for [`continue_path`]. Its query string is public
/// information (the client, its redirect and a PKCE challenge), never a secret.
pub fn continue_cookie(request: &AuthorizeRequest, secure: bool) -> HeaderValue {
    let value: String = form_urlencoded::byte_serialize(request.to_query().as_bytes()).collect();
    HeaderValue::from_str(&format!("{CONTINUE_COOKIE}={value}{}; Max-Age={CONTINUE_TTL_SECONDS}", cookie_flags(secure)))
        .expect("form-encoded cookie values are header-safe")
}

pub fn clear_continue_cookie(secure: bool) -> HeaderValue {
    HeaderValue::from_str(&format!("{CONTINUE_COOKIE}={}; Max-Age=0", cookie_flags(secure)))
        .expect("constant cookie string")
}

/// The authorization path to return to, if the request carries the cookie.
pub fn continue_path(headers: &HeaderMap) -> Option<String> {
    let value = headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|line| {
        line.split(';')
            .map(str::trim)
            .find_map(|kv| kv.strip_prefix(CONTINUE_COOKIE).and_then(|rest| rest.strip_prefix('=')))
    })?;
    let query: String = form_urlencoded::parse(value.as_bytes()).next().map(|(k, _)| k.into_owned())?;
    let request = AuthorizeRequest::from_query(&query);
    (request.client_id.is_some() && query.len() <= 4096).then(|| format!("{AUTHORIZE_PATH}?{}", request.to_query()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn server() -> Server {
        Server::new("https://example.test/", "Example", &["/mcp", "/api/v1"], Some("/docs/mcp"))
    }

    #[test]
    fn metadata_documents_describe_the_endpoints() {
        let server = server();
        let metadata = server.metadata();
        assert_eq!(metadata["issuer"], "https://example.test");
        assert_eq!(metadata["authorization_endpoint"], "https://example.test/oauth/authorize");
        assert_eq!(metadata["token_endpoint"], "https://example.test/oauth/token");
        assert_eq!(metadata["registration_endpoint"], "https://example.test/oauth/register");
        assert_eq!(metadata["revocation_endpoint"], "https://example.test/oauth/revoke");
        assert_eq!(metadata["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(metadata["grant_types_supported"], json!(["authorization_code", "refresh_token"]));
        assert_eq!(metadata["token_endpoint_auth_methods_supported"][0], "none");
        assert_eq!(metadata["service_documentation"], "https://example.test/docs/mcp");
        let resource = server.resource_metadata(Some("/mcp"));
        assert_eq!(resource["resource"], "https://example.test/mcp");
        assert_eq!(resource["authorization_servers"], json!(["https://example.test"]));
        assert_eq!(resource["resource_name"], "Example");
        assert_eq!(server.resource_metadata(None)["resource"], "https://example.test");
        assert_eq!(server.resource_metadata_path("/mcp"), "/.well-known/oauth-protected-resource/mcp");
        assert_eq!(
            server.www_authenticate("example", "/mcp"),
            "Bearer realm=\"example\", resource_metadata=\"https://example.test/.well-known/oauth-protected-resource/mcp\""
        );
        assert!(server.accepts_resource("https://example.test/mcp"));
        assert!(!server.accepts_resource("https://example.test/mcp/"));
    }

    #[test]
    fn redirect_uris_are_https_loopback_http_or_private_schemes() {
        for uri in [
            "https://claude.ai/api/mcp/auth_callback",
            "http://localhost:3334/callback",
            "http://127.0.0.1/cb?x=1",
            "http://[::1]:9/cb",
            "cursor://anysphere.cursor-retrieval/oauth/callback",
            "com.example.app:/oauth",
        ] {
            assert!(valid_redirect_uri(uri), "{uri}");
        }
        for uri in [
            "http://example.com/cb",
            "https://example.com/cb#fragment",
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "/relative",
            "not a url",
            "https://",
            "",
        ] {
            assert!(!valid_redirect_uri(uri), "{uri}");
        }
        assert!(redirect_uri_matches("https://a.test/cb", "https://a.test/cb"));
        assert!(!redirect_uri_matches("https://a.test/cb", "https://a.test/cb/"));
        assert!(!redirect_uri_matches("https://a.test/cb", "https://a.test:444/cb"));
        assert!(redirect_uri_matches("http://localhost:1234/cb", "http://localhost:5678/cb"));
        assert!(redirect_uri_matches("http://127.0.0.1/cb", "http://127.0.0.1:8080/cb"));
        assert!(!redirect_uri_matches("http://localhost:1234/cb", "http://localhost:1234/other"));
        assert!(!redirect_uri_matches("http://localhost:1234/cb", "http://127.0.0.1:1234/cb"));
        assert!(!redirect_uri_matches("http://localhost:1234/cb?a=1", "http://localhost:1234/cb?a=2"));
    }

    #[test]
    fn pkce_uses_s256_over_the_verifier() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cN"));
        assert!(!pkce_matches("short", challenge));
        assert!(!pkce_matches(&"a".repeat(129), challenge));
        assert!(valid_code_challenge(challenge));
        assert!(!valid_code_challenge("has space in it which is not allowed at all, no way, none"));
    }

    #[test]
    fn requests_round_trip_through_fields_queries_and_the_continue_cookie() {
        let query = "response_type=code&client_id=abc&redirect_uri=https%3A%2F%2Fa.test%2Fcb%3Fx%3D1&state=s%20t&code_challenge=c&code_challenge_method=S256&resource=https%3A%2F%2Fexample.test%2Fmcp";
        let request = AuthorizeRequest::from_query(query);
        assert_eq!(request.redirect_uri.as_deref(), Some("https://a.test/cb?x=1"));
        assert_eq!(request.state.as_deref(), Some("s t"));
        assert_eq!(request.fields().len(), 7);
        assert_eq!(AuthorizeRequest::from_query(&request.to_query()), request);
        let cookie = continue_cookie(&request, true);
        let cookie = cookie.to_str().unwrap();
        assert!(cookie.starts_with("oauth_continue=") && cookie.contains("; Secure") && cookie.contains("Max-Age=900"));
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.split(';').next().unwrap().parse().unwrap());
        assert_eq!(continue_path(&headers), Some(format!("/oauth/authorize?{}", request.to_query())));
        headers.insert(header::COOKIE, "oauth_continue=state%3Donly".parse().unwrap());
        assert_eq!(continue_path(&headers), None, "no client_id, nothing to continue");
        assert!(clear_continue_cookie(false).to_str().unwrap().contains("Max-Age=0"));
        let url = error_url(&server(), "https://a.test/cb?x=1", Some("s t"), "access_denied", "The user declined.");
        assert_eq!(
            url,
            "https://a.test/cb?x=1&error=access_denied&error_description=The+user+declined.&iss=https%3A%2F%2Fexample.test&state=s+t"
        );
    }

    async fn body(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn post_token(pool: &Pool, headers: HeaderMap, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
        let mut form = form_urlencoded::Serializer::new(String::new());
        for (k, v) in pairs {
            form.append_pair(k, v);
        }
        body(token(pool, &server(), &headers, form.finish().as_bytes()).await).await
    }

    async fn user(pool: &Pool, email: &str) -> User {
        crate::users::sign_up(pool, email, "correct-horse".into()).await.unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn registration_validates_metadata_and_issues_credentials(pool: Pool) {
        let (status, error) = body(register(&pool, b"nope").await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "invalid_client_metadata");
        for (request, error) in [
            (json!({}), "invalid_redirect_uri"),
            (json!({ "redirect_uris": ["http://example.com/cb"] }), "invalid_redirect_uri"),
            (
                json!({ "redirect_uris": ["https://a.test/cb"], "token_endpoint_auth_method": "private_key_jwt" }),
                "invalid_client_metadata",
            ),
            (json!({ "redirect_uris": ["https://a.test/cb"], "grant_types": ["implicit"] }), "invalid_client_metadata"),
            (
                json!({ "redirect_uris": ["https://a.test/cb"], "grant_types": ["refresh_token"] }),
                "invalid_client_metadata",
            ),
            (json!({ "redirect_uris": ["https://a.test/cb"], "response_types": ["token"] }), "invalid_client_metadata"),
            (
                json!({ "redirect_uris": ["https://a.test/cb"], "client_uri": "http://a.test" }),
                "invalid_client_metadata",
            ),
            (
                json!({ "redirect_uris": ["https://a.test/cb"], "client_name": "x".repeat(101) }),
                "invalid_client_metadata",
            ),
        ] {
            let (status, response) = body(register(&pool, request.to_string().as_bytes()).await).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{request}");
            assert_eq!(response["error"], error, "{request}: {response}");
        }
        let public = json!({
            "client_name": "  Claude  ", "redirect_uris": ["https://claude.ai/cb", "http://localhost:1/cb"],
            "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"],
            "client_uri": "https://claude.ai", "scope": "mcp", "ignored": true
        });
        let (status, response) = body(register(&pool, public.to_string().as_bytes()).await).await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
        assert_eq!(response["client_id"].as_str().unwrap().len(), 32);
        assert!(response.get("client_secret").is_none());
        assert_eq!(response["client_name"], "Claude");
        assert_eq!(response["client_uri"], "https://claude.ai");
        assert_eq!(response["scope"], "mcp");
        assert_eq!(response["response_types"], json!(["code"]));
        assert!(response["client_id_issued_at"].is_number());
        let confidential = json!({ "redirect_uris": ["https://chatgpt.com/cb"] });
        let (status, response) = body(register(&pool, confidential.to_string().as_bytes()).await).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(response["client_name"], DEFAULT_CLIENT_NAME);
        assert_eq!(response["token_endpoint_auth_method"], "client_secret_basic");
        assert_eq!(response["client_secret"].as_str().unwrap().len(), 64);
        assert_eq!(response["client_secret_expires_at"], 0);
        assert_eq!(response["grant_types"], json!(["authorization_code"]));
        let stored = find_client(&pool, response["client_id"].as_str().unwrap()).await.unwrap().unwrap();
        assert!(!stored.is_public() && !stored.may_refresh());
        assert_eq!(stored.secret_hash.as_deref(), Some(hash(response["client_secret"].as_str().unwrap()).as_slice()));
        assert!(find_client(&pool, "missing").await.unwrap().is_none());
    }

    async fn public_client(pool: &Pool, redirect_uri: &str) -> String {
        let request = json!({
            "client_name": "Agent", "redirect_uris": [redirect_uri], "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"]
        });
        let (status, response) = body(register(pool, request.to_string().as_bytes()).await).await;
        assert_eq!(status, StatusCode::CREATED);
        response["client_id"].as_str().unwrap().to_owned()
    }

    fn request(client_id: &str, redirect_uri: &str, challenge: &str) -> AuthorizeRequest {
        AuthorizeRequest {
            response_type: Some("code".into()),
            client_id: Some(client_id.into()),
            redirect_uri: Some(redirect_uri.into()),
            state: Some("xyz".into()),
            scope: Some("mcp".into()),
            code_challenge: Some(challenge.into()),
            code_challenge_method: Some("S256".into()),
            resource: Some("https://example.test/mcp".into()),
        }
    }

    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn query_of(url: &str) -> Vec<(String, String)> {
        form_pairs(url.split_once('?').unwrap().1.as_bytes())
    }

    async fn untrusted(pool: &Pool, server: &Server, request: AuthorizeRequest) -> String {
        match validate(pool, server, &request).await {
            Err(AuthorizeError::Untrusted(message)) => message.to_owned(),
            other => panic!("expected an untrusted request, got {other:?}"),
        }
    }

    /// The error a redirect carries, after checking it went to the client with `state` and `iss`.
    async fn redirected(pool: &Pool, server: &Server, request: AuthorizeRequest) -> &'static str {
        match validate(pool, server, &request).await {
            Err(AuthorizeError::Redirect { url, error }) => {
                assert!(url.starts_with("http://localhost:7777/cb?"), "{url}");
                let pairs = query_of(&url);
                assert_eq!(first(&pairs, "error"), Some(error));
                assert_eq!(first(&pairs, "state"), Some("xyz"));
                assert_eq!(first(&pairs, "iss"), Some("https://example.test"));
                error
            }
            other => panic!("expected a redirect, got {other:?}"),
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn authorization_requests_are_checked_before_any_redirect(pool: Pool) {
        let server = server();
        let client = public_client(&pool, "http://localhost:1/cb").await;
        let base = request(&client, "http://localhost:7777/cb", CHALLENGE);
        let ok = validate(&pool, &server, &base).await.unwrap();
        assert_eq!(ok.redirect_uri, "http://localhost:7777/cb", "loopback ports float");
        assert_eq!(ok.scope, "mcp");
        assert_eq!(ok.resource.as_deref(), Some("https://example.test/mcp"));
        assert!(
            untrusted(&pool, &server, AuthorizeRequest { client_id: None, ..base.clone() }).await.contains("client_id")
        );
        assert!(
            untrusted(&pool, &server, AuthorizeRequest { client_id: Some("nope".into()), ..base.clone() })
                .await
                .contains("Unknown")
        );
        assert!(
            untrusted(
                &pool,
                &server,
                AuthorizeRequest { redirect_uri: Some("https://evil.test/cb".into()), ..base.clone() }
            )
            .await
            .contains("redirect_uri")
        );
        assert!(
            untrusted(&pool, &server, AuthorizeRequest { state: Some("s".repeat(2049)), ..base.clone() })
                .await
                .contains("state")
        );
        let omitted = validate(&pool, &server, &AuthorizeRequest { redirect_uri: None, ..base.clone() }).await.unwrap();
        assert_eq!(omitted.redirect_uri, "http://localhost:1/cb", "a single registered URI is the default");
        assert_eq!(
            redirected(&pool, &server, AuthorizeRequest { response_type: None, ..base.clone() }).await,
            "unsupported_response_type"
        );
        assert_eq!(
            redirected(&pool, &server, AuthorizeRequest { code_challenge: None, ..base.clone() }).await,
            "invalid_request"
        );
        assert_eq!(
            redirected(&pool, &server, AuthorizeRequest { code_challenge: Some("short".into()), ..base.clone() }).await,
            "invalid_request"
        );
        assert_eq!(
            redirected(
                &pool,
                &server,
                AuthorizeRequest { code_challenge_method: Some("plain".into()), ..base.clone() }
            )
            .await,
            "invalid_request"
        );
        assert_eq!(
            redirected(&pool, &server, AuthorizeRequest { scope: Some("s".repeat(257)), ..base.clone() }).await,
            "invalid_scope"
        );
        assert_eq!(
            redirected(
                &pool,
                &server,
                AuthorizeRequest { resource: Some("https://other.test/".into()), ..base.clone() }
            )
            .await,
            "invalid_target"
        );
        let denied = ok.denied_url(&server);
        assert_eq!(first(&query_of(&denied), "error"), Some("access_denied"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn codes_become_rotating_tokens_that_authenticate_and_revoke(pool: Pool) {
        let server = server();
        let alice = user(&pool, "alice@example.com").await;
        let client = public_client(&pool, "http://localhost:1/cb").await;
        let authorization =
            validate(&pool, &server, &request(&client, "http://localhost:1/cb", CHALLENGE)).await.unwrap();
        let url = authorization.code_url(&pool, &server, alice.id).await.unwrap();
        let pairs = query_of(&url);
        let code = first(&pairs, "code").unwrap().to_owned();
        assert_eq!(code.len(), 64);
        assert_eq!(first(&pairs, "state"), Some("xyz"));
        assert_eq!(first(&pairs, "iss"), Some("https://example.test"));
        assert!(!url.contains("iss=&"), "{url}");

        // The token endpoint insists on the client, the verifier and the redirect.
        let exchange = |extra: Vec<(&'static str, String)>| {
            let code = code.clone();
            let client = client.clone();
            let pool = pool.clone();
            async move {
                let mut pairs: Vec<(&str, &str)> = vec![
                    ("grant_type", "authorization_code"),
                    ("client_id", &client),
                    ("code", &code),
                    ("code_verifier", VERIFIER),
                    ("redirect_uri", "http://localhost:1/cb"),
                ];
                let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                for (k, v) in extra {
                    pairs.retain(|(name, _)| *name != k);
                    if !v.is_empty() {
                        pairs.push((k, v));
                    }
                }
                post_token(&pool, HeaderMap::new(), &pairs).await
            }
        };
        let (status, error) = exchange(vec![("client_id", "nope".into())]).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_client")));
        let (status, error) = exchange(vec![("grant_type", "password".into())]).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unsupported_grant_type")));
        let (status, error) = exchange(vec![("code_verifier", String::new())]).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_request")));
        let (status, error) = exchange(vec![("redirect_uri", "http://localhost:2/cb".into())]).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
        assert!(error["error_description"].as_str().unwrap().contains("redirect_uri"));
        // A failed exchange consumed the code: single use, whatever the reason.
        let (status, error) = exchange(vec![]).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));

        // A fresh code with the wrong verifier, then a good one.
        let url = authorization.code_url(&pool, &server, alice.id).await.unwrap();
        let code = first(&query_of(&url), "code").unwrap().to_owned();
        let bad = [
            ("grant_type", "authorization_code"),
            ("client_id", &client),
            ("code", &code),
            ("code_verifier", CHALLENGE),
        ];
        let (status, error) = post_token(&pool, HeaderMap::new(), &bad).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
        assert!(error["error_description"].as_str().unwrap().contains("code_verifier"));
        let url = authorization.code_url(&pool, &server, alice.id).await.unwrap();
        let code = first(&query_of(&url), "code").unwrap().to_owned();
        let good = [
            ("grant_type", "authorization_code"),
            ("client_id", &client),
            ("code", &code),
            ("code_verifier", VERIFIER),
            ("resource", "https://example.test/mcp"),
        ];
        let (status, tokens) = post_token(&pool, HeaderMap::new(), &good).await;
        assert_eq!(status, StatusCode::OK, "{tokens}");
        assert_eq!(tokens["token_type"], "Bearer");
        assert_eq!(tokens["expires_in"], ACCESS_TTL.whole_seconds());
        assert_eq!(tokens["scope"], "mcp");
        let access = tokens["access_token"].as_str().unwrap().to_owned();
        let refresh = tokens["refresh_token"].as_str().unwrap().to_owned();
        assert!(access.starts_with(ACCESS_PREFIX) && refresh.starts_with(REFRESH_PREFIX));
        assert_eq!(authenticate(&pool, &access).await.unwrap().unwrap().email, "alice@example.com");
        assert!(authenticate(&pool, &refresh).await.unwrap().is_none(), "refresh tokens never authenticate");
        assert!(authenticate(&pool, "sro_nope").await.unwrap().is_none());
        let grants = grants_for_user(&pool, alice.id).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].client_name, "Agent");
        assert!(grants[0].last_used_at.is_some(), "authenticating records usage");
        assert!(grants[0].expires_at > OffsetDateTime::now_utc() + Duration::days(29));

        // Refreshing rotates: the old pair dies, the new one works.
        let (status, rotated) = post_token(
            &pool,
            HeaderMap::new(),
            &[("grant_type", "refresh_token"), ("client_id", &client), ("refresh_token", &refresh)],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{rotated}");
        let access2 = rotated["access_token"].as_str().unwrap().to_owned();
        let refresh2 = rotated["refresh_token"].as_str().unwrap().to_owned();
        assert_ne!(access2, access);
        assert!(authenticate(&pool, &access).await.unwrap().is_none(), "the old access token is gone");
        assert!(authenticate(&pool, &access2).await.unwrap().is_some());
        let (status, error) = post_token(
            &pool,
            HeaderMap::new(),
            &[("grant_type", "refresh_token"), ("client_id", &client), ("refresh_token", &refresh)],
        )
        .await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
        assert_eq!(grants_for_user(&pool, alice.id).await.unwrap().len(), 1, "rotation replaces, never adds");

        // Another client cannot refresh or revoke this grant.
        let other = public_client(&pool, "http://localhost:2/cb").await;
        let (status, _) = post_token(
            &pool,
            HeaderMap::new(),
            &[("grant_type", "refresh_token"), ("client_id", &other), ("refresh_token", &refresh2)],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let form = format!("client_id={other}&token={access2}");
        assert_eq!(revoke(&pool, &HeaderMap::new(), form.as_bytes()).await.status(), StatusCode::OK);
        assert!(authenticate(&pool, &access2).await.unwrap().is_some(), "someone else's revoke is a no-op");
        let form = format!("client_id={client}&token={refresh2}");
        assert_eq!(revoke(&pool, &HeaderMap::new(), form.as_bytes()).await.status(), StatusCode::OK);
        assert!(authenticate(&pool, &access2).await.unwrap().is_none(), "revoking by refresh token drops the grant");
        assert!(grants_for_user(&pool, alice.id).await.unwrap().is_empty());
        let form = format!("client_id={client}");
        assert_eq!(revoke(&pool, &HeaderMap::new(), form.as_bytes()).await.status(), StatusCode::BAD_REQUEST);
    }

    fn as_refs<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
        pairs.iter().map(|(k, v)| (*k, v.as_str())).collect()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn confidential_clients_authenticate_with_their_secret(pool: Pool) {
        let server = server();
        let bob = user(&pool, "bob@example.com").await;
        let registration =
            json!({ "redirect_uris": ["https://chatgpt.com/cb"], "token_endpoint_auth_method": "client_secret_basic" });
        let (_, registered) = body(register(&pool, registration.to_string().as_bytes()).await).await;
        let id = registered["client_id"].as_str().unwrap().to_owned();
        let secret = registered["client_secret"].as_str().unwrap().to_owned();
        let authorization = validate(&pool, &server, &request(&id, "https://chatgpt.com/cb", CHALLENGE)).await.unwrap();
        let fresh_code = || async {
            let url = authorization.code_url(&pool, &server, bob.id).await.unwrap();
            first(&query_of(&url), "code").unwrap().to_owned()
        };
        let grant = |code: String| {
            vec![("grant_type", "authorization_code"), ("code", "placeholder"), ("code_verifier", VERIFIER)]
                .into_iter()
                .map(|(k, v)| (k, if k == "code" { code.clone() } else { v.to_owned() }))
                .collect::<Vec<_>>()
        };
        // Without credentials, with a wrong secret, and with the secret in the body (method mismatch is tolerated).
        let code = fresh_code().await;
        let (status, error) = post_token(&pool, HeaderMap::new(), &as_refs(&grant(code))).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_client")));
        let mut wrong = HeaderMap::new();
        wrong
            .insert(header::AUTHORIZATION, format!("Basic {}", STANDARD.encode(format!("{id}:nope"))).parse().unwrap());
        let (status, error) = post_token(&pool, wrong, &as_refs(&grant(fresh_code().await))).await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_client")));
        let mut basic = HeaderMap::new();
        basic.insert(
            header::AUTHORIZATION,
            format!("Basic {}", STANDARD.encode(format!("{id}:{secret}"))).parse().unwrap(),
        );
        let (status, tokens) = post_token(&pool, basic, &as_refs(&grant(fresh_code().await))).await;
        assert_eq!(status, StatusCode::OK, "{tokens}");
        assert!(tokens.get("refresh_token").is_none(), "no refresh_token grant was registered");
        let mut pairs = grant(fresh_code().await);
        pairs.push(("client_id", id.clone()));
        pairs.push(("client_secret", secret.clone()));
        let (status, tokens) = post_token(&pool, HeaderMap::new(), &as_refs(&pairs)).await;
        assert_eq!(status, StatusCode::OK, "{tokens}");
        assert_eq!(authenticate(&pool, tokens["access_token"].as_str().unwrap()).await.unwrap().unwrap().id, bob.id);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn expiry_bounds_and_cleanup(pool: Pool) {
        let server = server();
        let carol = user(&pool, "carol@example.com").await;
        let client = public_client(&pool, "http://localhost:1/cb").await;
        let authorization =
            validate(&pool, &server, &request(&client, "http://localhost:1/cb", CHALLENGE)).await.unwrap();
        // An expired code is refused and purged; the client that only has expired rows survives for 90 days.
        let url = authorization.code_url(&pool, &server, carol.id).await.unwrap();
        let code = first(&query_of(&url), "code").unwrap().to_owned();
        sqlx::query!("UPDATE oauth_codes SET expires_at = now() - interval '1 minute'").execute(&pool).await.unwrap();
        let (status, error) = post_token(
            &pool,
            HeaderMap::new(),
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client),
                ("code", &code),
                ("code_verifier", VERIFIER),
            ],
        )
        .await;
        assert_eq!((status, error["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_grant")));
        assert!(error["error_description"].as_str().unwrap().contains("expired"));
        assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM oauth_codes").fetch_one(&pool).await.unwrap().n, 0);
        // More grants than the cap: the oldest go.
        for _ in 0..(MAX_TOKENS_PER_USER + 2) {
            let url = authorization.code_url(&pool, &server, carol.id).await.unwrap();
            let code = first(&query_of(&url), "code").unwrap().to_owned();
            let (status, _) = post_token(
                &pool,
                HeaderMap::new(),
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &client),
                    ("code", &code),
                    ("code_verifier", VERIFIER),
                ],
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let grants = grants_for_user(&pool, carol.id).await.unwrap();
        assert_eq!(grants.len() as i64, MAX_TOKENS_PER_USER);
        // Expired access without refresh stops authenticating; expired refresh is purged; the settings list hides lapsed grants.
        sqlx::query!(
            "UPDATE oauth_tokens SET access_expires_at = now() - interval '1 second' WHERE id = $1",
            grants[0].id
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(grants_for_user(&pool, carol.id).await.unwrap().len() as i64, MAX_TOKENS_PER_USER, "refreshable");
        sqlx::query!(
            "UPDATE oauth_tokens SET refresh_expires_at = now() - interval '1 second' WHERE id = $1",
            grants[0].id
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(grants_for_user(&pool, carol.id).await.unwrap().len() as i64, MAX_TOKENS_PER_USER - 1);
        assert!(revoke_grant(&pool, grants[1].id, carol.id).await.unwrap());
        assert!(!revoke_grant(&pool, grants[1].id, carol.id).await.unwrap());
        let dave = user(&pool, "dave@example.com").await;
        assert!(!revoke_grant(&pool, grants[2].id, dave.id).await.unwrap(), "not yours");
        let stale = public_client(&pool, "http://localhost:3/cb").await;
        sqlx::query!("UPDATE oauth_clients SET created_at = now() - interval '91 days' WHERE id = $1", stale)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query!("UPDATE oauth_clients SET created_at = now() - interval '91 days' WHERE id = $1", client)
            .execute(&pool)
            .await
            .unwrap();
        // A code nobody exchanged before it expired is purged too.
        authorization.code_url(&pool, &server, carol.id).await.unwrap();
        sqlx::query!("UPDATE oauth_codes SET expires_at = now() - interval '1 minute'").execute(&pool).await.unwrap();
        let purged = purge_expired(&pool).await.unwrap();
        assert_eq!(purged, 1 + 1 + 1, "one expired code, one lapsed token, one stale client");
        assert!(find_client(&pool, &stale).await.unwrap().is_none());
        assert!(find_client(&pool, &client).await.unwrap().is_some(), "a client with live tokens stays");
        assert_eq!(purge_expired(&pool).await.unwrap(), 0);
    }
}
