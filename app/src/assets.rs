//! Embedded static assets (`/assets/*`). URLs carry a content hash
//! (`AppState::asset_url`), so responses are immutable for a year.

use crate::Assets;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

pub async fn serve(Path(path): Path<String>, headers: HeaderMap) -> Response {
    let Some(file) = Assets::get(&path) else {
        return crate::errors::not_found(&headers, || StatusCode::NOT_FOUND.into_response());
    };
    let mime = content_type(&path);
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
        ],
        file.data,
    )
        .into_response()
}

/// Content type by file extension. Extend this when you add a new kind of
/// asset; unknown extensions are served as `application/octet-stream`.
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("json" | "webmanifest") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::content_type;

    #[test]
    fn maps_known_extensions_and_falls_back() {
        assert_eq!(content_type("style.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("img/logo.svg"), "image/svg+xml");
        assert_eq!(content_type("fonts/inter.woff2"), "font/woff2");
        assert_eq!(content_type("archive.tar.gz"), "application/octet-stream");
        assert_eq!(content_type("README"), "application/octet-stream");
    }
}
