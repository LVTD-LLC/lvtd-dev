//! Account settings. Identity always comes from CurrentUser, never form fields.
#[cfg(feature = "api")]
use crate::api_keys;
#[cfg(feature = "storage")]
use crate::avatars;
use crate::{
    AppError, SharedState,
    routes::html,
    views::{self, Page},
};
use app_auth::passkeys::{self, Passkey};
use app_auth::two_factor;
use app_auth::{CurrentUser, User, password, sessions, users};
use axum::{
    extract::{Form, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub struct Profile {
    pub name: String,
    pub email: String,
    /// Created through a sign-in provider and has no usable password. The column
    /// exists in every build (migration 0016); only the `google` and `github`
    /// features act on it.
    pub passwordless: bool,
    /// The first linked sign-in provider this build supports, which a
    /// passwordless account confirms with.
    pub provider: Option<String>,
}

impl Profile {
    /// The provider a passwordless account confirms sensitive changes with
    /// instead of a password. Without a provider feature every account is
    /// treated as a password account.
    pub fn confirm_with(&self) -> Option<&str> {
        self.provider.as_deref().filter(|_| cfg!(feature = "social-login") && self.passwordless)
    }
}

pub async fn profile(pool: &PgPool, id: Uuid) -> Result<Profile, sqlx::Error> {
    #[cfg(feature = "social-login")]
    let providers = crate::social_login::supported_ids();
    #[cfg(not(feature = "social-login"))]
    let providers: Vec<String> = Vec::new();
    sqlx::query_as!(
        Profile,
        "SELECT name, email, passwordless, (SELECT provider FROM social_identities i WHERE i.user_id = users.id AND i.provider = ANY($2) ORDER BY i.created_at, i.provider LIMIT 1) AS provider FROM users WHERE id = $1",
        id,
        &providers
    )
    .fetch_one(pool)
    .await
}

/// `None` when `supplied` proves the signed-in user is acting: their current password,
/// or for a passwordless account (`google` or `github` feature) a provider confirmation
/// completed on this session in the last five minutes. Otherwise the message telling them what to
/// do, `password_message` for password accounts. Callers that need the row locked
/// take `FOR UPDATE` on `users` first, in the same connection.
pub(crate) async fn prove_owner(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    supplied: String,
    token: Option<&str>,
    password_message: &'static str,
) -> anyhow::Result<Option<&'static str>> {
    let account = sqlx::query!("SELECT password_hash, passwordless FROM users WHERE id = $1", user_id)
        .fetch_one(&mut *conn)
        .await?;
    #[cfg(feature = "social-login")]
    if account.passwordless {
        drop(supplied);
        let confirmed = crate::social_login::confirmed(&mut *conn, user_id, token).await?;
        return Ok((!confirmed).then_some(crate::social_login::CONFIRM_MESSAGE));
    }
    #[cfg(not(feature = "social-login"))]
    let _ = (token, account.passwordless);
    let ok = supplied.len() <= password::MAX_LEN && password::verify(supplied, Some(account.password_hash)).await?;
    Ok((!ok).then_some(password_message))
}

/// [`prove_owner`] for handlers outside a transaction.
pub(crate) async fn owner_confirmed(
    state: &SharedState,
    user: &User,
    supplied: String,
    headers: &HeaderMap,
    password_message: &'static str,
) -> Result<Option<&'static str>, AppError> {
    let mut conn = state.pool.acquire().await?;
    Ok(prove_owner(&mut conn, user.id, supplied, sessions::token_from_headers(headers), password_message).await?)
}

#[derive(Default, Deserialize)]
pub struct Notice {
    saved: Option<String>,
    revoked: Option<String>,
    /// `?{provider}=…` outcomes from `social_login.rs`.
    #[cfg(feature = "social-login")]
    #[serde(flatten)]
    providers: std::collections::HashMap<String, String>,
}

impl Notice {
    fn message(&self) -> Option<std::borrow::Cow<'static, str>> {
        #[cfg(feature = "social-login")]
        if let Some(message) = crate::social_login::settings_notice(&self.providers) {
            return Some(message.into());
        }
        match (self.saved.as_deref(), self.revoked.as_deref()) {
            (Some("1"), _) => Some("Your profile has been saved."),
            (Some("passkey"), _) => Some("Your passkey is ready. Use it next time you sign in."),
            (Some("passkey-removed"), _) => Some("The passkey has been removed."),
            (Some("avatar"), _) => Some("Your profile picture has been updated."),
            (Some("avatar-removed"), _) => Some("Your profile picture has been removed."),
            (_, Some("1")) => Some("The API key has been revoked. Requests using it now fail."),
            (_, Some("app")) => Some("The application has been disconnected. Its tokens no longer work."),
            _ => None,
        }
        .map(Into::into)
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SettingsForm {
    Profile {
        name: String,
        email: String,
        #[serde(default)]
        password: String,
    },
    Delete {
        #[serde(default)]
        password: String,
        #[serde(default)]
        confirmation: String,
    },
    #[cfg(feature = "api")]
    CreateApiKey {
        #[serde(default)]
        name: String,
    },
    #[cfg(feature = "api")]
    RevokeApiKey {
        #[serde(default)]
        id: String,
    },
    /// Revoke an OAuth grant listed under Connected apps.
    #[cfg(feature = "api")]
    DisconnectApp {
        #[serde(default)]
        id: String,
    },
    /// The add-passkey form without JavaScript; `passkeys.js` intercepts it otherwise.
    Passkey {
        #[serde(default)]
        name: String,
        #[serde(default)]
        password: String,
    },
    RemovePasskey {
        #[serde(default)]
        id: String,
    },
    /// Uploads post to `/settings/avatar` (multipart); removal is a plain form action.
    #[cfg(feature = "storage")]
    RemoveAvatar,
}

/// Messages for one rendering of the settings page.
#[derive(Default)]
pub struct Flash<'a> {
    pub error: Option<&'a str>,
    pub notice: Option<&'a str>,
    /// Re-opens the deletion form with the typed confirmation on failure.
    pub confirmation: Option<&'a str>,
    /// An API key created by this request, shown exactly once.
    pub new_key: Option<&'a str>,
}

/// What the settings page shows besides the profile.
pub struct View<'a> {
    pub flash: Flash<'a>,
    #[cfg(feature = "api")]
    pub keys: &'a [api_keys::ApiKey],
    /// Applications connected through OAuth, newest first.
    #[cfg(feature = "api")]
    pub grants: &'a [app_auth::oauth_server::Grant],
    pub passkeys: &'a [Passkey],
    pub two_factor: two_factor::Status,
    /// The current profile picture, where the browser can load it.
    #[cfg(feature = "storage")]
    pub avatar_url: Option<String>,
    /// Sign-in providers to show: configured or already linked.
    #[cfg(feature = "social-login")]
    pub sign_in_methods: Vec<crate::social_login::Method>,
}

/// The settings page with one problem, for handlers outside this module
/// (the avatar upload, the sign-in provider forms) that fail the same way the form actions do.
#[cfg(any(feature = "storage", feature = "social-login"))]
pub(crate) async fn problem(
    state: &SharedState,
    user: &User,
    status: StatusCode,
    message: &str,
) -> Result<Response, AppError> {
    let profile = profile(&state.pool, user.id).await?;
    render(state, user, &profile, status, Flash { error: Some(message), ..Flash::default() }).await
}

async fn render(
    state: &SharedState,
    user: &User,
    profile: &Profile,
    status: StatusCode,
    flash: Flash<'_>,
) -> Result<Response, AppError> {
    #[cfg(feature = "api")]
    let keys = api_keys::list_for_user(&state.pool, user.id).await?;
    #[cfg(feature = "api")]
    let grants = app_auth::oauth_server::grants_for_user(&state.pool, user.id).await?;
    let passkeys = passkeys::list(&state.pool, user.id).await?;
    let two_factor = two_factor::status(&state.pool, user.id).await?;
    let view = views::settings::View {
        flash,
        #[cfg(feature = "api")]
        keys: &keys,
        #[cfg(feature = "api")]
        grants: &grants,
        passkeys: &passkeys,
        two_factor,
        #[cfg(feature = "storage")]
        avatar_url: user.avatar_key.as_deref().map(|key| avatars::url(&state.storage, key)),
        #[cfg(feature = "social-login")]
        sign_in_methods: crate::social_login::methods(state, user.id).await?,
    };
    let body = maud::html! {
        (views::settings::page(profile, &view, user))
        script defer src=(state.asset_url("passkeys.js")) {}
    };
    Ok(html(
        status,
        views::layout(state, Page { title: "Account settings", path: "/settings", user: Some(user), body }),
    ))
}

pub async fn page(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    Query(notice): Query<Notice>,
) -> Result<Response, AppError> {
    let profile = profile(&state.pool, user.id).await?;
    let message = notice.message();
    render(&state, &user, &profile, StatusCode::OK, Flash { notice: message.as_deref(), ..Flash::default() }).await
}

#[derive(Debug, thiserror::Error)]
enum SettingsError {
    #[error("{0}")]
    Validation(&'static str),
    #[cfg(feature = "api")]
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
impl From<sqlx::Error> for SettingsError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

async fn update(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    email: &str,
    supplied_password: String,
    token: Option<&str>,
) -> Result<(), SettingsError> {
    let name = name.trim();
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(SettingsError::Validation("Use a name of 100 characters or fewer, without control characters."));
    }
    let email = users::normalize_email(email)
        .filter(|email| !email.chars().any(|c| c.is_whitespace() || c.is_control()) && email.matches('@').count() == 1)
        .ok_or(SettingsError::Validation("Enter a valid email address."))?;
    let mut tx = pool.begin().await?;
    let account = sqlx::query!("SELECT email FROM users WHERE id = $1 FOR UPDATE", id).fetch_one(&mut *tx).await?;
    let email_changed = email != account.email;
    if email_changed
        && let Some(message) =
            prove_owner(&mut tx, id, supplied_password, token, "Enter your current password to change your email.")
                .await?
    {
        return Err(SettingsError::Validation(message));
    }
    if let Err(error) =
        sqlx::query!("UPDATE users SET name = $2, email = $3, updated_at = now() WHERE id = $1", id, name, email)
            .execute(&mut *tx)
            .await
    {
        if error.as_database_error().is_some_and(|e| e.is_unique_violation()) {
            return Err(SettingsError::Validation("An account with that email already exists."));
        }
        return Err(error.into());
    }
    if email_changed {
        let hash = token.map(|t| Sha256::digest(t.as_bytes()).to_vec());
        sqlx::query!("DELETE FROM sessions WHERE user_id = $1 AND token_hash IS DISTINCT FROM $2", id, hash)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn delete(
    state: &SharedState,
    id: Uuid,
    supplied_password: String,
    confirmation: &str,
    token: Option<&str>,
) -> Result<(), SettingsError> {
    if confirmation != "DELETE" {
        return Err(SettingsError::Validation("Type DELETE to confirm account deletion."));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", id).fetch_one(&mut *tx).await?;
    if let Some(message) =
        prove_owner(&mut tx, id, supplied_password, token, "Enter your current password to delete your account.")
            .await?
    {
        return Err(SettingsError::Validation(message));
    }
    #[cfg(feature = "storage")]
    let avatar_key = sqlx::query_scalar!("SELECT avatar_key FROM users WHERE id = $1", id).fetch_one(&mut *tx).await?;
    // Serialize with billing checkout/webhook writes, even if Stripe is disabled now.
    // The user lock also prevents a new billing account being inserted during deletion.
    sqlx::query!("SELECT user_id FROM billing_accounts WHERE user_id = $1 FOR UPDATE", id)
        .fetch_optional(&mut *tx)
        .await?;
    let pending = sqlx::query!(
        "SELECT EXISTS(SELECT 1 FROM billing_checkouts WHERE user_id = $1 AND active) OR EXISTS(SELECT 1 FROM billing_subscriptions WHERE user_id = $1 AND status NOT IN ('canceled', 'incomplete_expired')) AS \"blocked!\"",
        id
    ).fetch_one(&mut *tx).await?.blocked;
    if pending {
        return Err(SettingsError::Validation(
            "Resolve pending payments and cancel subscriptions in Billing before deleting your account. If a payment or cancellation is still processing, wait or contact support.",
        ));
    }
    // Foreign keys cascade to sessions and local billing records.
    sqlx::query!("DELETE FROM users WHERE id = $1", id).execute(&mut *tx).await?;
    // Data in other systems goes through jobs, committed with the deletion.
    #[cfg(feature = "qdrant")]
    app_core::jobs::enqueue(
        &mut *tx,
        &crate::jobs::PurgeVectors { user_id: id },
        app_core::jobs::Enqueue { dedupe_key: Some(format!("purge_vectors:{id}")), ..Default::default() },
    )
    .await?;
    tx.commit().await?;
    // The stored picture is not referenced by anything now.
    #[cfg(feature = "storage")]
    avatars::discard(&state.storage, avatar_key).await;
    Ok(())
}

pub async fn submit(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    let mut profile = profile(&state.pool, user.id).await?;
    let mut confirmation_input = None;
    let result = match form {
        SettingsForm::Profile { name, email, password } => {
            profile.name = name;
            profile.email = email;
            update(
                &state.pool,
                user.id,
                &profile.name,
                &profile.email,
                password,
                sessions::token_from_headers(&headers),
            )
            .await
        }
        SettingsForm::Delete { password, confirmation } => {
            confirmation_input = Some(confirmation.clone());
            match delete(&state, user.id, password, &confirmation, sessions::token_from_headers(&headers)).await {
                Ok(()) => {
                    return Ok((
                        [(header::SET_COOKIE, sessions::clear_cookie(state.cfg.secure_cookies()))],
                        Redirect::to("/"),
                    )
                        .into_response());
                }
                Err(error) => Err(error),
            }
        }
        // The plaintext key exists only in this response, so it is rendered
        // directly instead of redirecting.
        #[cfg(feature = "api")]
        SettingsForm::CreateApiKey { name } => match api_keys::create(&state.pool, user.id, &name).await {
            Ok((_, token)) => {
                let flash = Flash { new_key: Some(&token), ..Flash::default() };
                return render(&state, &user, &profile, StatusCode::OK, flash).await;
            }
            Err(api_keys::CreateError::Other(error)) => return Err(error.into()),
            Err(error) => Err(SettingsError::Message(error.to_string())),
        },
        #[cfg(feature = "api")]
        SettingsForm::RevokeApiKey { id } => {
            let revoked = match Uuid::parse_str(id.trim()) {
                Ok(id) => api_keys::revoke(&state.pool, id, user.id).await?,
                Err(_) => false,
            };
            if revoked {
                return Ok(Redirect::to("/settings?revoked=1").into_response());
            }
            Err(SettingsError::Validation("That API key no longer exists."))
        }
        #[cfg(feature = "api")]
        SettingsForm::DisconnectApp { id } => {
            let revoked = match Uuid::parse_str(id.trim()) {
                Ok(id) => app_auth::oauth_server::revoke_grant(&state.pool, id, user.id).await?,
                Err(_) => false,
            };
            if revoked {
                return Ok(Redirect::to("/settings?revoked=app").into_response());
            }
            Err(SettingsError::Validation("That application is no longer connected."))
        }
        SettingsForm::Passkey { .. } => {
            Err(SettingsError::Validation("Adding a passkey needs JavaScript and a browser that supports passkeys."))
        }
        SettingsForm::RemovePasskey { id } => match id.parse::<Uuid>() {
            Ok(id) if passkeys::remove(&state.pool, user.id, id).await? => {
                return Ok(Redirect::to("/settings?saved=passkey-removed").into_response());
            }
            _ => Err(SettingsError::Validation("That passkey no longer exists.")),
        },
        #[cfg(feature = "storage")]
        SettingsForm::RemoveAvatar => {
            avatars::remove(&state, user.id).await?;
            return Ok(Redirect::to("/settings?saved=avatar-removed").into_response());
        }
    };
    match result {
        Ok(()) => Ok(Redirect::to("/settings?saved=1").into_response()),
        Err(SettingsError::Validation(error)) => {
            let flash = Flash { error: Some(error), confirmation: confirmation_input.as_deref(), ..Flash::default() };
            render(&state, &user, &profile, StatusCode::UNPROCESSABLE_ENTITY, flash).await
        }
        #[cfg(feature = "api")]
        Err(SettingsError::Message(error)) => {
            let flash = Flash { error: Some(&error), ..Flash::default() };
            render(&state, &user, &profile, StatusCode::UNPROCESSABLE_ENTITY, flash).await
        }
        Err(SettingsError::Other(error)) => Err(error.into()),
    }
}
