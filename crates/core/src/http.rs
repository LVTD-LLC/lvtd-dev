//! HTTP middleware and content negotiation shared by every app built on LVTD.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The git revision this binary was built from. Set by the Dockerfile's
/// `BUILD_REVISION` build-arg; deploy pipelines read it back from the
/// `x-deployment-revision` header to confirm the new version is live.
pub const BUILD_REVISION: &str = match option_env!("BUILD_REVISION") {
    Some(v) => v,
    None => "development",
};

pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert("x-deployment-revision", HeaderValue::from_static(BUILD_REVISION));
    resp
}

/// CSRF protection without tokens: state-changing requests must come from
/// our own origin. Modern browsers send `Sec-Fetch-Site`; older ones send
/// `Origin` on cross-site POSTs. Combined with `SameSite=Lax` session
/// cookies this closes cross-site form posts.
///
/// `site_url` is the public origin (`Config::site_url`).
pub fn same_origin_guard(site_url: String) -> impl Fn(Request, Next) -> SameOriginFuture + Clone {
    move |req, next| {
        let allowed = site_url.clone();
        Box::pin(async move {
            if req.method() == Method::GET || req.method() == Method::HEAD || req.method() == Method::OPTIONS {
                return next.run(req).await;
            }
            let headers = req.headers();
            let ok = match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
                Some("same-origin") | Some("none") => true,
                Some(_) => false,
                None => match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
                    Some(origin) => origin.trim_end_matches('/') == allowed,
                    // Non-browser clients (curl, tests) send neither header.
                    None => true,
                },
            };
            if ok { next.run(req).await } else { (StatusCode::FORBIDDEN, "cross-site request blocked").into_response() }
        })
    }
}

pub type SameOriginFuture = std::pin::Pin<Box<dyn Future<Output = Response> + Send>>;

/// Require an explicit Markdown media type, honor quality and specificity,
/// and prefer Markdown on a tie. Wildcards alone keep the browser default.
pub fn prefers_markdown(headers: &HeaderMap) -> bool {
    let mut markdown: Option<f32> = None;
    let mut html: Option<(u8, f32)> = None;
    for value in headers.get_all(header::ACCEPT) {
        let Ok(value) = value.to_str() else { continue };
        for range in value.split(',') {
            let mut parts = range.split(';');
            let media_type = parts.next().unwrap_or_default().trim();
            let mut quality = 1.0_f32;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=')
                    && name.trim().eq_ignore_ascii_case("q")
                {
                    quality = value.trim().parse::<f32>().ok().filter(|q| (0.0..=1.0).contains(q)).unwrap_or(0.0);
                }
            }
            if media_type.eq_ignore_ascii_case("text/markdown") {
                markdown = Some(markdown.map_or(quality, |q| q.max(quality)));
            }
            let specificity = if media_type.eq_ignore_ascii_case("text/html") {
                2
            } else if media_type.eq_ignore_ascii_case("text/*") {
                1
            } else if media_type == "*/*" {
                0
            } else {
                continue;
            };
            if html.is_none_or(|(s, q)| specificity > s || (specificity == s && quality > q)) {
                html = Some((specificity, quality));
            }
        }
    }
    markdown.is_some_and(|q| q > 0.0 && q >= html.map_or(0.0, |(_, q)| q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_explicit_markdown_with_quality_preferences() {
        for (accept, expected) in [
            ("text/markdown", true),
            ("TEXT/MARKDOWN; charset=utf-8", true),
            ("text/html, text/markdown", true),
            ("text/markdown;q=0.8, text/html;q=0.5", true),
            ("text/markdown;q=0.5, text/html;q=0.9", false),
            ("text/markdown;q=0, */*;q=1", false),
            ("text/markdown;q=0.5, */*;q=1, text/html;q=0.1", true),
            ("text/markdown;q=0.5, text/*;q=0.8", false),
            ("text/markdown;q=NaN", false),
            ("text/markdown;q=2", false),
            ("text/markdown;q=invalid", false),
            ("application/text/markdown", false),
            ("text/markdown-extra", false),
            ("text/html,application/xhtml+xml,*/*;q=0.8", false),
            ("*/*", false),
            ("", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
            assert_eq!(prefers_markdown(&headers), expected, "{accept}");
        }
        let mut headers = HeaderMap::new();
        assert!(!prefers_markdown(&headers));
        headers.append(header::ACCEPT, HeaderValue::from_static("text/html;q=0.5"));
        headers.append(header::ACCEPT, HeaderValue::from_static("text/markdown"));
        assert!(prefers_markdown(&headers));
    }
}
