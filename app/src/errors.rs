//! Content negotiation for missing resources. HTML remains the default.

#[cfg(not(feature = "sentry-errors"))]
pub use app_core::AppError;
use app_core::http::prefers_markdown;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
#[cfg(feature = "sentry-errors")]
use std::sync::Arc;

/// Application error boundary. Keep the original error on the response so the
/// observability middleware can report its source chain without exposing it.
#[cfg(feature = "sentry-errors")]
pub enum AppError {
    Internal(anyhow::Error),
    Status(StatusCode, String),
}

#[cfg(feature = "sentry-errors")]
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}

#[cfg(feature = "sentry-errors")]
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::Internal(error) => {
                tracing::error!(error = ?error, "request failed");
                let mut response = (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
                response.extensions_mut().insert(Arc::new(error));
                response
            }
            Self::Status(status, message) => (status, message).into_response(),
        }
    }
}

const NOT_FOUND: &str = "# 404 — Not found\n\nThe requested page or resource could not be found. Check the address or use the links below.\n\n- [Home](/)\n- [Sitemap](/sitemap.xml)\n";

pub(crate) fn not_found(headers: &HeaderMap, fallback: impl FnOnce() -> Response) -> Response {
    let mut response = if prefers_markdown(headers) {
        (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], NOT_FOUND).into_response()
    } else {
        fallback()
    };
    response.headers_mut().append(header::VARY, HeaderValue::from_static("Accept"));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}
