//! Prepared Markdown for public HTML pages. Construct once when registering a route.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};

/// Only pass public, anonymous HTML here. The browser response can still be personalized.
/// A real HTML parser preserves headings, links, lists, tables and code without maintaining
/// a second version of the content. No parsing or conversion runs on requests.
#[derive(Clone)]
pub struct PublicPage {
    markdown: Bytes,
}

impl PublicPage {
    pub fn new(html: &str) -> Self {
        let markdown = htmd::HtmlToMarkdown::builder()
            .skip_tags(vec!["head", "script", "style", "svg", "nav", "footer", "noscript"])
            .build()
            .convert(html)
            .expect("public page HTML must convert to Markdown");
        assert!(!markdown.trim().is_empty(), "public pages must have readable content");
        Self { markdown: markdown.into() }
    }

    /// Negotiate without rendering HTML for Markdown clients. Both representations vary
    /// on Accept, and existing Vary values on the HTML response are preserved.
    pub fn respond(&self, headers: &HeaderMap, html: impl FnOnce() -> Response) -> Response {
        let mut response = if crate::http::prefers_markdown(headers) {
            (
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8"), (header::CACHE_CONTROL, "private, no-store")],
                self.markdown.clone(),
            )
                .into_response()
        } else {
            html()
        };
        response.headers_mut().append(header::VARY, HeaderValue::from_static("Accept"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::Html;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn preserves_content_and_omits_browser_chrome() {
        let page = PublicPage::new(
            r#"<!doctype html><html><head><title>Hidden title</title></head><body>
            <nav>Hidden navigation</nav><main><h1>A &amp; B</h1><p>Read <a href="/docs">the guide</a>.</p>
            <h2>Features</h2><ul><li>One</li><li>Two</li></ul><p><strong>Bold</strong> and <em>italic</em>.</p>
            <pre><code class="language-rust">let x = 1 &lt; 2;</code></pre>
            <table><thead><tr><th>Name</th><th>Value</th></tr></thead><tbody><tr><td>A</td><td>1</td></tr></tbody></table>
            <script>Hidden script</script><style>Hidden style</style><svg><text>Hidden icon</text></svg>
            <noscript>Hidden fallback</noscript></main><footer>Hidden footer</footer></body></html>"#,
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/markdown"));
        let response = page.respond(&headers, || panic!("Markdown must not render HTML"));
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/markdown; charset=utf-8");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(response.headers()[header::VARY], "Accept");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&bytes).unwrap();
        for content in [
            "A & B",
            "[the guide](/docs)",
            "Features",
            "One",
            "Two",
            "**Bold**",
            "*italic*",
            "let x = 1 < 2;",
            "Name",
            "Value",
        ] {
            assert!(body.contains(content), "missing {content}: {body}");
        }
        assert!(!body.contains("Hidden"), "{body}");
        assert!(!body.contains("<h1>"));
        assert!(body.contains("```"));
    }

    #[tokio::test]
    async fn html_preferences_keep_the_original_response_and_vary_values() {
        let page = PublicPage::new("<h1>Public content</h1>");
        for accept in ["text/html", "*/*", "text/markdown;q=0", "text/html;q=1, text/markdown;q=0.5", ""] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
            let response = page.respond(&headers, || {
                (
                    [(header::VARY, "Accept-Encoding"), (header::SET_COOKIE, "example=value")],
                    Html("<h1>Personalized HTML</h1>"),
                )
                    .into_response()
            });
            assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
            assert_eq!(response.headers()[header::SET_COOKIE], "example=value");
            let vary: Vec<_> = response.headers().get_all(header::VARY).iter().map(|v| v.to_str().unwrap()).collect();
            assert_eq!(vary, ["Accept-Encoding", "Accept"]);
            assert_eq!(response.into_body().collect().await.unwrap().to_bytes(), "<h1>Personalized HTML</h1>");
        }
    }

    #[test]
    #[should_panic(expected = "public pages must have readable content")]
    fn empty_content_fails_at_startup() {
        PublicPage::new("<script>invisible</script>");
    }
}
