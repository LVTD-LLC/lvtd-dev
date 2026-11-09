//! Passkey ceremonies: the JSON endpoints `assets/passkeys.js` talks to.
//!
//! Registration is offered only in account settings, to a signed-in user who
//! has just confirmed their password (or, for a passwordless account, with
//! its sign-in provider); it never creates accounts. Sign-in is anonymous and ends in the
//! same session cookie as the password form.

use crate::{AppError, SharedState, routes};
use app_auth::CurrentUser;
use app_auth::passkeys::{self, AuthenticationResponse, PasskeyError, RegistrationResponse};
use axum::Json;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct RegisterOptionsForm {
    #[serde(default)]
    password: String,
}

fn reply(status: StatusCode, value: Value) -> Response {
    (status, [(header::CACHE_CONTROL, "private, no-store")], Json(value)).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    reply(status, json!({ "error": message }))
}

/// Client mistakes become a message the page shows; everything else is a 500.
fn rejected(error: PasskeyError) -> Result<Response, AppError> {
    match error {
        PasskeyError::Rejected(rejected) => Ok(self::error(StatusCode::UNPROCESSABLE_ENTITY, &rejected.to_string())),
        PasskeyError::Other(error) => Err(error.into()),
    }
}

/// Step 1 of adding a passkey: confirm the account owner, then issue options.
pub async fn register_options(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
    Form(form): Form<RegisterOptionsForm>,
) -> Result<Response, AppError> {
    let message = "Enter your current password to add a passkey.";
    if let Some(message) = crate::settings::owner_confirmed(&state, &user, form.password, &headers, message).await? {
        return Ok(error(StatusCode::UNPROCESSABLE_ENTITY, message));
    }
    let profile = crate::settings::profile(&state.pool, user.id).await?;
    match passkeys::start_registration(&state.pool, &state.rp, &user, &profile.name).await {
        Ok(options) => Ok(reply(StatusCode::OK, options)),
        Err(error) => rejected(error),
    }
}

/// Step 2: verify the browser's attestation response and store the passkey.
pub async fn register(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    Json(response): Json<RegistrationResponse>,
) -> Result<Response, AppError> {
    match passkeys::finish_registration(&state.pool, &state.rp, user.id, response).await {
        Ok(passkey) => {
            #[cfg(feature = "posthog-product")]
            state.track(&user, "passkey added", json!({ "backed_up": passkey.backed_up }));
            Ok(reply(
                StatusCode::CREATED,
                json!({ "id": passkey.id, "name": passkey.name, "redirect": "/settings?saved=passkey" }),
            ))
        }
        Err(error) => rejected(error),
    }
}

/// Anonymous: a fresh challenge for a discoverable-credential sign-in.
pub async fn sign_in_options(State(state): State<SharedState>) -> Result<Response, AppError> {
    match passkeys::start_authentication(&state.pool, &state.rp).await {
        Ok(options) => Ok(reply(StatusCode::OK, options)),
        Err(error) => rejected(error),
    }
}

/// Verify the assertion and start a session, exactly like the password form.
pub async fn sign_in(
    State(state): State<SharedState>,
    Json(response): Json<AuthenticationResponse>,
) -> Result<Response, AppError> {
    match passkeys::finish_authentication(&state.pool, &state.rp, response).await {
        Ok(user) => {
            #[cfg(feature = "posthog")]
            crate::analytics::record_user(&user);
            let cookie: HeaderValue = routes::session_cookie(&state, &user).await?;
            #[cfg(feature = "posthog-product")]
            state.track(&user, "user signed in", json!({ "method": "passkey" }));
            let mut response = reply(StatusCode::OK, json!({ "redirect": "/dashboard" }));
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            Ok(response)
        }
        Err(error) => rejected(error),
    }
}
