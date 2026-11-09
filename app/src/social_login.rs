//! Optional sign-in through external identity providers ("Continue with
//! Google", the `google` and `github` features): sign-up and sign-in, explicit
//! linking from account settings, and session-bound confirmation for accounts
//! that have no password.
//!
//! This module owns the routes, the `social_identities`, `social_login_flows`
//! and `social_login_confirmations` tables (migration 0016, present in every
//! build) and the rules below. A provider is a [`Client`]: it builds the
//! authorization URL and turns a callback code into an [`Identity`]. Google
//! and GitHub ship with the template. Adding one takes a `Client`, a Cargo
//! feature that turns on `social-login`, an entry in [`SUPPORTED`], its
//! configuration in [`Providers::from_env`] and a button icon in the views;
//! never a schema change.
//!
//! - A provider never links to an existing account by email: a matching
//!   address is told to sign in and connect the provider from settings after
//!   a password check. Each provider decides which emails it is authoritative
//!   for, and only those may create accounts.
//! - A passwordless account (created through a provider) keeps a hash of a
//!   discarded random password, so the shared auth schema is untouched, and
//!   confirms sensitive settings with a fresh round trip through a linked
//!   provider on the same session instead (five minutes). It cannot
//!   disconnect its last provider.
//! - An enrolled authenticator app still applies: provider sign-in ends in the
//!   two-factor challenge, never directly in a session.
//!
//! This is the client side of OAuth and OpenID Connect: signing people in with
//! an account they hold elsewhere. It is unrelated to issuing OAuth tokens.
#[cfg(feature = "github")]
pub mod github;
#[cfg(feature = "google")]
pub mod google;

use crate::{AppError, SharedState};
use app_auth::{CurrentUser, MaybeUser, User, password, sessions, users};
use axum::{
    Router,
    extract::{Form, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{collections::HashMap, future::Future, pin::Pin};
use uuid::Uuid;

/// Every provider this build supports, configured or not, in display order:
/// `(id, name)`. The id appears in routes, notices and `social_identities`.
/// Rows for any other provider are ignored: they cannot sign anyone in.
pub const SUPPORTED: &[(&str, &str)] = &[
    #[cfg(feature = "google")]
    ("google", "Google"),
    #[cfg(feature = "github")]
    ("github", "GitHub"),
];

/// The ids in [`SUPPORTED`], for SQL filters.
pub fn supported_ids() -> Vec<String> {
    SUPPORTED.iter().map(|(id, _)| (*id).to_owned()).collect()
}

const FLOW_COOKIE: &str = "social_login_flow";
/// What a passwordless account is told when a sensitive action needs a fresh confirmation.
pub const CONFIRM_MESSAGE: &str = "Confirm with your sign-in provider in account settings, then try again.";

/// The display name for a provider id.
pub fn name(id: &str) -> &'static str {
    SUPPORTED.iter().find(|(p, _)| *p == id).map_or("your sign-in provider", |(_, name)| name)
}

/// What a provider proved about the person who finished its consent screen.
pub struct Identity {
    /// The provider's stable account identifier, never an email or username.
    pub subject: String,
    /// An email the provider is authoritative for, if any. Only this may create an account.
    pub signup_email: Option<String>,
}

/// A failed exchange. Provider responses, codes and secrets never enter errors.
#[derive(Debug)]
pub struct Rejected;

pub type Exchange<'a> = Pin<Box<dyn Future<Output = Result<Identity, Rejected>> + Send + 'a>>;

/// One identity provider's half of the authorization code flow.
pub trait Client: Send + Sync {
    /// Where to send the browser. Send `state` back unchanged, the S256 PKCE
    /// challenge of `verifier`, and `nonce` when the provider issues ID tokens.
    fn authorization_url(&self, state: &str, nonce: &str, verifier: &str) -> String;
    /// Redeem a callback code for a verified identity.
    fn identify<'a>(&'a self, code: &'a str, verifier: &'a str, nonce: &'a str) -> Exchange<'a>;
}

/// The configured providers. Unconfigured ones show no buttons and their
/// routes answer with an "unavailable" notice.
#[derive(Default)]
pub struct Providers(Vec<(&'static str, Box<dyn Client>)>);
// Each `Providers::from_env` arm reads `{ID}_CLIENT_ID` and `{ID}_CLIENT_SECRET`.

impl Providers {
    /// Each provider is off when its variables are all blank; partial configuration fails startup.
    pub fn from_env(site_url: &str) -> anyhow::Result<Self> {
        let mut providers = Self::default();
        #[cfg(feature = "google")]
        if let Some(config) = google::Config::from_env()? {
            providers.insert("google", google::Client::new(config, site_url)?);
        }
        #[cfg(feature = "github")]
        if let Some(config) = github::Config::from_env()? {
            providers.insert("github", github::Client::new(config, site_url)?);
        }
        Ok(providers)
    }
    pub fn insert(&mut self, id: &'static str, client: impl Client + 'static) {
        assert!(SUPPORTED.iter().any(|(p, _)| *p == id), "unsupported provider {id}");
        self.0.retain(|(p, _)| *p != id);
        self.0.push((id, Box::new(client)));
    }
    pub fn get(&self, id: &str) -> Option<&dyn Client> {
        self.0.iter().find(|(p, _)| *p == id).map(|(_, client)| client.as_ref())
    }
    /// Configured providers in display order, as `(id, name)`.
    pub fn available(&self) -> Vec<(&'static str, &'static str)> {
        SUPPORTED.iter().copied().filter(|(id, _)| self.get(id).is_some()).collect()
    }
}

/// Parse a provider's JSON response: success status only, at most 64 KiB.
pub(crate) async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, Rejected> {
    if !response.status().is_success() {
        return Err(Rejected);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Rejected)? {
        if bytes.len() + chunk.len() > 65_536 {
            return Err(Rejected);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Rejected)
}

/// `SITE_URL/auth/{id}/callback`, registered exactly with the provider. Only
/// HTTPS origins, or plain HTTP on localhost for development.
pub fn redirect_uri(site_url: &str, id: &str) -> anyhow::Result<String> {
    let origin = reqwest::Url::parse(site_url)?;
    anyhow::ensure!(
        origin.origin().ascii_serialization() == site_url.trim_end_matches('/')
            && (origin.scheme() == "https"
                || (origin.scheme() == "http" && matches!(origin.host_str(), Some("localhost" | "127.0.0.1")))),
        "{} authentication requires HTTPS or a localhost development origin",
        name(id)
    );
    Ok(format!("{}/auth/{id}/callback", site_url.trim_end_matches('/')))
}

/// `POST /auth/{id}`, `GET /auth/{id}/callback` and `POST /settings/{id}` for
/// every supported provider. Unconfigured providers still answer with a notice.
pub fn routes(mut router: Router<SharedState>) -> Router<SharedState> {
    for &(id, _) in SUPPORTED {
        router = router
            .route(
                &format!("/auth/{id}"),
                post(move |state: State<SharedState>, user: MaybeUser| start(state, user, id)),
            )
            .route(
                &format!("/auth/{id}/callback"),
                get(move |state: State<SharedState>, user: MaybeUser, headers: HeaderMap, query: Query<Callback>| {
                    callback(state, user, headers, query, id)
                }),
            )
            .route(
                &format!("/settings/{id}"),
                post(
                    move |state: State<SharedState>,
                          user: CurrentUser,
                          headers: HeaderMap,
                          form: Form<SettingsForm>| { settings(state, user, headers, form, id) },
                ),
            );
    }
    router
}

/// The provider id a request path belongs to, for `/auth/{id}`,
/// `/auth/{id}/callback` and `/settings/{id}`.
fn provider_path(path: &str) -> Option<&'static str> {
    let rest = match path.strip_prefix("/auth/") {
        Some(rest) => rest.strip_suffix("/callback").unwrap_or(rest),
        None => path.strip_prefix("/settings/")?,
    };
    SUPPORTED.iter().find(|(id, _)| *id == rest).map(|(id, _)| *id)
}

/// The form posts that must carry same-origin evidence: `/auth/{id}` and `/settings/{id}`.
pub fn is_form_path(path: &str) -> bool {
    provider_path(path).is_some() && !path.ends_with("/callback")
}

fn random() -> anyhow::Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes)?;
    Ok(hex::encode(bytes))
}
fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}
fn browser_token(headers: &HeaderMap) -> Option<&str> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|s| s.split(';')).find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == FLOW_COOKIE && value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
    })
}
fn flow_cookie(provider: &str, token: &str, secure: bool) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}={token}; Path=/auth/{provider}/callback; HttpOnly; SameSite=Lax; Max-Age={}{}",
        if token.is_empty() { 0 } else { 600 },
        if secure { "; Secure" } else { "" }
    ))
    .expect("hex cookie")
}

/// Runs outside the shared security headers so callbacks never send their code
/// as a Referer. Also covers malformed callbacks rejected by extractors.
pub async fn private_response(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let private = provider_path(request.uri().path()).is_some();
    let mut response = next.run(request).await;
    if private {
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
        response.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        response.headers_mut().insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    }
    response
}

/// The provider and outcome named by a `?{id}=…` notice.
fn notice(query: &HashMap<String, String>) -> Option<(&'static str, &str)> {
    SUPPORTED.iter().find_map(|(id, name)| Some((*name, query.get(*id)?.as_str())))
}

/// The message for `/sign-in?{id}=…` after a flow ends without a session.
pub fn sign_in_notice(query: &HashMap<String, String>) -> Option<String> {
    let (name, kind) = notice(query)?;
    Some(match kind {
        "unavailable" => format!("{name} sign-in is temporarily unavailable. Please try again later."),
        "cancelled" => format!("{name} sign-in was cancelled. You can try again."),
        "link" => format!(
            "Sign in with your existing method, or create an account with email first, then connect {name} in account settings."
        ),
        "failed" => format!("We couldn’t complete {name} sign-in. Please start again."),
        _ => return None,
    })
}

/// The message for `/settings?{id}=…` after a linking or confirmation flow.
pub fn settings_notice(query: &HashMap<String, String>) -> Option<String> {
    let (name, kind) = notice(query)?;
    Some(match kind {
        "linked" => format!("{name} is connected. You can use it next time you sign in."),
        "confirmed" => {
            format!("{name} confirmed your identity. You can make sensitive changes for the next five minutes.")
        }
        "disconnected" => format!("{name} is disconnected. Your other sessions have been signed out."),
        "conflict" => format!("That {name} account or this account is already connected."),
        "wrong-account" => format!("Choose the {name} account connected to this account."),
        _ => return None,
    })
}

/// One provider's row in the settings page's sign-in section.
pub struct Method {
    pub id: &'static str,
    pub name: &'static str,
    /// Configured, so it can start a flow now.
    pub available: bool,
    pub linked: bool,
}

/// Providers to show in settings: configured ones, and linked ones even when unconfigured.
pub async fn methods(state: &SharedState, user_id: Uuid) -> Result<Vec<Method>, sqlx::Error> {
    let linked = linked(&state.pool, user_id).await?;
    Ok(SUPPORTED
        .iter()
        .map(|&(id, name)| Method {
            id,
            name,
            available: state.social_login.get(id).is_some(),
            linked: linked.iter().any(|p| p == id),
        })
        .filter(|m| m.available || m.linked)
        .collect())
}

/// The account's supported providers.
async fn linked<'e>(
    executor: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT provider FROM social_identities WHERE user_id = $1 AND provider = ANY($2)",
        user_id,
        &supported_ids()
    )
    .fetch_all(executor)
    .await
}

async fn begin(
    state: &SharedState,
    provider: &'static str,
    purpose: &str,
    account: Option<(&User, &str)>,
) -> Result<Response, AppError> {
    let Some(client) = state.social_login.get(provider) else {
        return Ok(login_notice(provider, "unavailable"));
    };
    let token = random()?;
    let state_token = random()?;
    let nonce = random()?;
    let verifier = random()?;
    let state_hash = hash(&state_token);
    let browser_hash = hash(&token);
    let user_id = account.map(|(u, _)| u.id);
    let session_hash = account.map(|(_, token)| hash(token));
    sqlx::query!("DELETE FROM social_login_flows WHERE expires_at <= now()").execute(&state.pool).await?;
    sqlx::query!("INSERT INTO social_login_flows (state_hash, provider, browser_hash, nonce, verifier, purpose, user_id, session_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        state_hash, provider, browser_hash, nonce, verifier, purpose, user_id, session_hash).execute(&state.pool).await?;
    let url = client.authorization_url(&state_token, &nonce, &verifier);
    Ok(([(header::SET_COOKIE, flow_cookie(provider, &token, state.cfg.secure_cookies()))], Redirect::to(&url))
        .into_response())
}

async fn start(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    provider: &'static str,
) -> Result<Response, AppError> {
    if user.is_some() {
        return Ok(Redirect::to("/dashboard").into_response());
    }
    begin(&state, provider, "login", None).await
}

#[derive(Deserialize)]
pub struct SettingsForm {
    action: String,
    #[serde(default)]
    password: String,
}
async fn settings(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
    provider: &'static str,
) -> Result<Response, AppError> {
    let Some(token) = sessions::token_from_headers(&headers) else {
        return Err(AppError::Status(StatusCode::UNAUTHORIZED, "Sign in first.".into()));
    };
    let name = name(provider);
    let profile = crate::settings::profile(&state.pool, user.id).await?;
    let connected = linked(&state.pool, user.id).await?;
    let this = connected.iter().any(|p| p == provider);
    let allowed = match form.action.as_str() {
        "confirm" => profile.passwordless && this,
        "link" => true,
        "disconnect" => this && !(profile.passwordless && connected.len() == 1),
        _ => false,
    };
    if !allowed {
        let message = if profile.passwordless && this {
            format!("Keep {name} connected: it is your account’s sign-in method.")
        } else {
            format!("That {name} action isn’t available for your account.")
        };
        return crate::settings::problem(&state, &user, StatusCode::UNPROCESSABLE_ENTITY, &message).await;
    }
    if form.action == "confirm" {
        return begin(&state, provider, "confirm", Some((&user, token))).await;
    }
    let authorized = if profile.passwordless {
        confirmed(&state.pool, user.id, Some(token)).await?
    } else {
        form.password.len() <= password::MAX_LEN
            && users::verify_credentials(&state.pool, &user.email, form.password).await?.is_some()
    };
    if !authorized {
        let message = if profile.passwordless {
            CONFIRM_MESSAGE.to_owned()
        } else {
            format!("Enter your current password to change your {name} connection.")
        };
        return crate::settings::problem(&state, &user, StatusCode::UNPROCESSABLE_ENTITY, &message).await;
    }
    if form.action == "disconnect" {
        let mut tx = state.pool.begin().await?;
        sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user.id).fetch_one(&mut *tx).await?;
        sqlx::query!("DELETE FROM social_identities WHERE user_id = $1 AND provider = $2", user.id, provider)
            .execute(&mut *tx)
            .await?;
        // Re-checked under the user lock: two concurrent disconnects must not
        // both remove a passwordless account's last provider.
        if profile.passwordless && linked(&mut *tx, user.id).await?.is_empty() {
            tx.rollback().await?;
            let message = format!("Keep {name} connected: it is your account’s sign-in method.");
            return crate::settings::problem(&state, &user, StatusCode::UNPROCESSABLE_ENTITY, &message).await;
        }
        sqlx::query!("DELETE FROM social_login_flows WHERE user_id = $1 AND provider = $2", user.id, provider)
            .execute(&mut *tx)
            .await?;
        sqlx::query!("DELETE FROM two_factor_challenges WHERE user_id = $1", user.id).execute(&mut *tx).await?;
        let session_hash = hash(token);
        sqlx::query!("DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2", user.id, session_hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(Redirect::to(&format!("/settings?{provider}=disconnected")).into_response());
    }
    begin(&state, provider, "link", Some((&user, token))).await
}

#[derive(Deserialize)]
pub struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
async fn callback(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    headers: HeaderMap,
    Query(query): Query<Callback>,
    provider: &'static str,
) -> Result<Response, AppError> {
    let mut response = finish(&state, provider, user.as_ref(), &headers, query).await?;
    response.headers_mut().append(header::SET_COOKIE, flow_cookie(provider, "", state.cfg.secure_cookies()));
    Ok(response)
}
fn login_notice(provider: &str, kind: &str) -> Response {
    Redirect::to(&format!("/sign-in?{provider}={kind}")).into_response()
}

async fn finish(
    state: &SharedState,
    provider: &'static str,
    user: Option<&User>,
    headers: &HeaderMap,
    query: Callback,
) -> Result<Response, AppError> {
    let Some(client) = state.social_login.get(provider) else {
        return Ok(login_notice(provider, "unavailable"));
    };
    let (Some(state_token), Some(browser)) = (query.state, browser_token(headers)) else {
        return Ok(login_notice(provider, "failed"));
    };
    if state_token.len() != 64 {
        return Ok(login_notice(provider, "failed"));
    }
    let state_hash = hash(&state_token);
    let browser_hash = hash(browser);
    let flow = sqlx::query!("DELETE FROM social_login_flows WHERE state_hash = $1 AND browser_hash = $2 AND provider = $3 AND expires_at > now() RETURNING nonce, verifier, purpose, user_id, session_hash",
        state_hash, browser_hash, provider).fetch_optional(&state.pool).await?;
    let Some(flow) = flow else {
        return Ok(login_notice(provider, "failed"));
    };
    if query.error.is_some() {
        return Ok(login_notice(provider, "cancelled"));
    }
    if flow.purpose != "login"
        && (user.map(|u| u.id) != flow.user_id || sessions::token_from_headers(headers).map(hash) != flow.session_hash)
    {
        return Ok(login_notice(provider, "failed"));
    }
    if flow.purpose == "login" && user.is_some() {
        return Ok(Redirect::to("/dashboard").into_response());
    }
    let Some(code) = query.code.filter(|c| !c.is_empty() && c.len() <= 4096) else {
        return Ok(login_notice(provider, "failed"));
    };
    let Ok(identity) = client.identify(&code, &flow.verifier, &flow.nonce).await else {
        return Ok(login_notice(provider, "failed"));
    };
    if flow.purpose == "login" {
        let Some((account, created)) = account(&state.pool, provider, &identity).await? else {
            return Ok(login_notice(provider, "link"));
        };
        #[cfg(feature = "posthog")]
        crate::analytics::record_user(&account);
        let challenge = app_auth::two_factor::is_enabled(&state.pool, account.id).await?;
        return issue_login(state, provider, &account, &identity.subject, challenge, created).await;
    }
    let id = flow.user_id.expect("database constraint");
    let session_hash = flow.session_hash.expect("database constraint");
    let mut tx = state.pool.begin().await?;
    // Serialize identity changes and re-check the session after the network call.
    let exists = sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", id).fetch_optional(&mut *tx).await?;
    if exists.is_none() {
        return Ok(login_notice(provider, "failed"));
    }
    let valid = sqlx::query!(
        "SELECT id FROM sessions WHERE user_id = $1 AND token_hash = $2 AND expires_at > now() FOR UPDATE",
        id,
        session_hash
    )
    .fetch_optional(&mut *tx)
    .await?;
    if valid.is_none() {
        return Ok(login_notice(provider, "failed"));
    }
    if flow.purpose == "link" {
        let added = sqlx::query!(
            "INSERT INTO social_identities (user_id, provider, subject) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING user_id",
            id,
            provider,
            identity.subject
        )
        .fetch_optional(&mut *tx)
        .await?;
        if added.is_none() {
            return Ok(Redirect::to(&format!("/settings?{provider}=conflict")).into_response());
        }
    } else {
        if !is_linked(&mut *tx, id, provider, &identity.subject).await? {
            return Ok(Redirect::to(&format!("/settings?{provider}=wrong-account")).into_response());
        }
        sqlx::query!("INSERT INTO social_login_confirmations (session_hash) VALUES ($1) ON CONFLICT (session_hash) DO UPDATE SET expires_at = now() + interval '5 minutes'", session_hash).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let outcome = if flow.purpose == "link" { "linked" } else { "confirmed" };
    Ok(Redirect::to(&format!("/settings?{provider}={outcome}")).into_response())
}

async fn is_linked<'e>(
    executor: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    user_id: Uuid,
    provider: &str,
    subject: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query!(
        "SELECT user_id FROM social_identities WHERE user_id = $1 AND provider = $2 AND subject = $3",
        user_id,
        provider,
        subject
    )
    .fetch_optional(executor)
    .await?
    .is_some())
}

/// Recheck after insertion: disconnect deletes existing credentials while holding
/// the user lock, but an in-flight INSERT can wait on that lock and complete
/// afterward. Never return such a credential to the browser.
async fn issue_login(
    state: &SharedState,
    provider: &'static str,
    user: &User,
    subject: &str,
    challenge: bool,
    created: bool,
) -> Result<Response, AppError> {
    let ttl = time::Duration::days(state.cfg.session_ttl_days);
    let token = if challenge {
        app_auth::two_factor::start_challenge(&state.pool, user.id).await?
    } else {
        sessions::create(&state.pool, user.id, ttl).await?
    };
    if !is_linked(&state.pool, user.id, provider, subject).await? {
        if challenge {
            let token_hash = hash(&token);
            sqlx::query!("DELETE FROM two_factor_challenges WHERE token_hash = $1", token_hash)
                .execute(&state.pool)
                .await?;
        } else {
            sessions::delete(&state.pool, &token).await?;
        }
        return Ok(login_notice(provider, "failed"));
    }
    let (cookie, destination) = if challenge {
        (app_auth::two_factor::cookie(&token, state.cfg.secure_cookies()), "/sign-in/two-factor")
    } else {
        #[cfg(feature = "posthog-product")]
        state.track(
            user,
            if created { "user signed up" } else { "user signed in" },
            serde_json::json!({ "method": provider }),
        );
        let _ = created;
        (sessions::session_cookie(&token, ttl, state.cfg.secure_cookies()), "/dashboard")
    };
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to(destination)).into_response())
}

async fn existing_account(pool: &PgPool, provider: &str, subject: &str) -> anyhow::Result<Option<(User, bool)>> {
    let row =
        sqlx::query!("SELECT user_id FROM social_identities WHERE provider = $1 AND subject = $2", provider, subject)
            .fetch_optional(pool)
            .await?;
    match row {
        Some(row) => Ok(users::find(pool, row.user_id).await?.map(|user| (user, false))),
        None => Ok(None),
    }
}

async fn account(pool: &PgPool, provider: &str, identity: &Identity) -> anyhow::Result<Option<(User, bool)>> {
    if let Some(account) = existing_account(pool, provider, &identity.subject).await? {
        return Ok(Some(account));
    }
    let Some(email) = &identity.signup_email else {
        return Ok(None);
    };
    // Preserve the shared crate's non-null password contract without ever
    // giving a passwordless account a usable password.
    let password_hash = password::hash(random()?).await?;
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query!("INSERT INTO users (id, email, password_hash, passwordless) VALUES ($1, $2, $3, true) ON CONFLICT (email) DO NOTHING RETURNING id",
        id, email, password_hash).fetch_optional(&mut *tx).await?;
    if inserted.is_none() {
        tx.rollback().await?;
        return existing_account(pool, provider, &identity.subject).await;
    }
    let linked = sqlx::query!(
        "INSERT INTO social_identities (user_id, provider, subject) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING user_id",
        id,
        provider,
        identity.subject
    )
    .fetch_optional(&mut *tx)
    .await?;
    if linked.is_none() {
        tx.rollback().await?;
        return existing_account(pool, provider, &identity.subject).await;
    }
    tx.commit().await?;
    Ok(users::find(pool, id).await?.map(|user| (user, true)))
}

/// Passwordless accounts confirm a newly completed provider round trip on this
/// exact session. Password accounts retain their existing password checks.
pub async fn confirmed<'e>(
    executor: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    user_id: Uuid,
    token: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(token) = token else {
        return Ok(false);
    };
    let session_hash = hash(token);
    Ok(sqlx::query!("SELECT s.id FROM sessions s JOIN social_login_confirmations c ON c.session_hash = s.token_hash JOIN users u ON u.id = s.user_id WHERE s.token_hash = $1 AND s.user_id = $2 AND s.expires_at > now() AND c.expires_at > now() AND u.passwordless", session_hash, user_id).fetch_optional(executor).await?.is_some())
}

#[cfg(test)]
mod tests;
