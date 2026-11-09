//! Axum extractors. Add `CurrentUser` to a handler's arguments and the
//! route is signed-in only; `MaybeUser` for pages that work either way.
//!
//! The app's state type implements [`AuthState`] so these extractors can
//! reach the pool without depending on the app crate.

use crate::sessions::{self, Session};
use crate::users::User;
use app_core::Pool;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};

pub trait AuthState: Send + Sync + 'static {
    fn pool(&self) -> &Pool;
    /// Where to send anonymous visitors who hit a signed-in-only route.
    fn sign_in_path(&self) -> &str {
        "/sign-in"
    }
}

impl<T: AuthState> AuthState for std::sync::Arc<T> {
    fn pool(&self) -> &Pool {
        (**self).pool()
    }
    fn sign_in_path(&self) -> &str {
        (**self).sign_in_path()
    }
}

/// The signed-in user. Rejects with a redirect to the sign-in page.
#[derive(Clone, Debug)]
pub struct CurrentUser(pub User);

/// The signed-in user if there is one.
#[derive(Clone, Debug)]
pub struct MaybeUser(pub Option<User>);

async fn resolve<S: AuthState>(parts: &Parts, state: &S) -> Option<Session> {
    let token = sessions::token_from_headers(&parts.headers)?;
    match sessions::lookup(state.pool(), token).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = ?e, "session lookup failed");
            None
        }
    }
}

impl<S: AuthState> FromRequestParts<S> for MaybeUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(MaybeUser(resolve(parts, state).await.map(|s| s.user)))
    }
}

impl<S: AuthState> FromRequestParts<S> for CurrentUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match resolve(parts, state).await {
            Some(s) => Ok(CurrentUser(s.user)),
            None => Err(Redirect::to(state.sign_in_path()).into_response()),
        }
    }
}
