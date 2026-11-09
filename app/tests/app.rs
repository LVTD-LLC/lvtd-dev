//! End-to-end tests through the router against a real Postgres database.
//! `#[sqlx::test]` creates a fresh database per test from `DATABASE_URL`
//! and applies the migrations, so tests never share state.

mod common;

use lvtd_dev as app;

use app_core::Config;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

#[cfg(feature = "auth")]
const SITE_NAME: &str = "LVTD";

fn app(pool: PgPool) -> Router {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    #[cfg(feature = "auth")]
    return common::mail_app(app::build(cfg, pool)).1;
    #[cfg(not(feature = "auth"))]
    app::build(cfg, pool).1
}

async fn send(router: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn form(path: &str, body: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::post(path)
        .header(header::ORIGIN, "http://test.local")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// The `session=...` pair from a Set-Cookie header, ready to send back.
#[cfg(feature = "auth")]
fn session_cookie(headers: &axum::http::HeaderMap) -> String {
    let raw = headers.get(header::SET_COOKIE).expect("set-cookie").to_str().unwrap();
    assert!(raw.contains("HttpOnly"), "{raw}");
    assert!(raw.contains("SameSite=Lax") || raw.contains("SameSite=Strict"), "{raw}");
    raw.split(';').next().unwrap().to_string()
}

#[sqlx::test(migrations = "../migrations")]
async fn public_pages_render(pool: PgPool) {
    let app = app(pool);
    let paths: &[&str] =
        if cfg!(feature = "auth") { &["/", "/sign-in", "/sign-up", "/healthz"] } else { &["/", "/healthz"] };
    for &path in paths {
        let (status, _, body) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        if path != "/healthz" {
            assert!(
                body.contains(&format!("<meta name=\"generator\" content=\"{}\">", app::views::GENERATOR)),
                "{path}"
            );
            assert!(
                body.contains(&format!(
                    "<a href=\"{}\">{}</a>",
                    app::views::attribution::URL,
                    app::views::attribution::CREDIT
                )),
                "{path}"
            );
        }
    }
    let (status, _, _) = send(&app, get("/nope", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../migrations")]
async fn sitemap_lists_enabled_public_pages(pool: PgPool) {
    let app = app(pool);
    let request = Request::get("/sitemap.xml").header(header::HOST, "untrusted.example").body(Body::empty()).unwrap();
    let (status, headers, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/xml; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    assert!(!headers.contains_key(header::SET_COOKIE));
    assert!(body.contains("<loc>http://test.local/</loc>"));
    assert_eq!(body.contains("<loc>http://test.local/blog</loc>"), cfg!(feature = "blog"));
    #[cfg(feature = "blog")]
    for post in app::blog::Blog::load("http://test.local").unwrap().posts {
        assert!(body.contains(&format!("<loc>http://test.local{}</loc>", post.path)));
    }
    assert!(body.contains("<loc>http://test.local/terms</loc>"));
    assert!(body.contains("<loc>http://test.local/privacy</loc>"));
    assert!(!body.contains("untrusted.example"));
    assert!(!body.contains("/dashboard"));
}

#[sqlx::test(migrations = "../migrations")]
async fn homepage_shows_one_idea_per_screen(pool: PgPool) {
    let app = app(pool);
    let (status, _, home) = send(&app, get("/", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(home.matches("<h1").count(), 1, "the hero is the only H1");
    assert_eq!(home.matches("class=\"screen-title\"").count(), 3);
    assert_eq!(home.matches("class=\"hero screen\"").count(), 1);
    // Customers read this page: no card grids or stack jargon in the placeholder.
    for absent in ["class=\"features\"", "class=\"eyebrow\"", "Axum", "Postgres"] {
        assert!(!home.contains(absent), "{absent}");
    }
    let cta = if cfg!(feature = "auth") { r#"href="/sign-up""# } else { r#"href="mailto:"# };
    assert_eq!(home.matches(&format!(r#"class="btn btn-primary btn-lg" {cta}"#)).count(), 2);
}

#[sqlx::test(migrations = "../migrations")]
async fn public_pages_share_as_large_cards(pool: PgPool) {
    let app = app(pool);
    let (_, _, home) = send(&app, get("/", None)).await;
    let description = format!("name=\"description\" content=\"{}\"", app::views::SITE_DESCRIPTION);
    assert!(home.contains(&description));
    assert!(home.contains("property=\"og:url\" content=\"http://test.local/\""));
    assert!(home.contains("name=\"twitter:card\" content=\"summary_large_image\""));
    let image = home.split("property=\"og:image\" content=\"").nth(1).unwrap().split('"').next().unwrap();
    assert!(image.starts_with("http://test.local/og/index.png?v="));
    let (status, headers, _) = send(&app, get(image.trim_start_matches("http://test.local"), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
}

#[cfg(any(feature = "blog", feature = "docs"))]
#[sqlx::test(migrations = "../migrations")]
async fn blog_and_docs_pages_get_their_own_cards(pool: PgPool) {
    let app = app(pool);
    let mut paths = Vec::new();
    #[cfg(feature = "blog")]
    paths.extend(["/blog".to_owned(), app::blog::Blog::load("http://test.local").unwrap().posts[0].path.clone()]);
    #[cfg(feature = "docs")]
    paths.extend(["/docs".to_owned(), app::docs::Docs::load().unwrap().pages[0].path.clone()]);
    let mut images = std::collections::HashSet::new();
    for path in paths {
        let (_, _, page) = send(&app, get(&path, None)).await;
        let image =
            page.split("property=\"og:image\" content=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
        assert!(image.starts_with(&format!("http://test.local/og{path}.png?v=")), "{path}: {image}");
        assert!(page.contains(&format!("name=\"twitter:image\" content=\"{image}\"")), "{path}");
        let (status, headers, _) = send(&app, get(image.trim_start_matches("http://test.local"), None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
        assert!(images.insert(image), "{path} shares another page's card");
    }
    let (status, _, _) = send(&app, get("/og/missing.png", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../migrations")]
async fn legal_pages_render_and_list_only_enabled_services(pool: PgPool) {
    let app = app(pool);
    let (_, _, home) = send(&app, get("/", None)).await;
    for (path, heading) in [("/terms", "Terms of Service"), ("/privacy", "Privacy Policy")] {
        let (status, _, body) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(&format!("<h1>{heading}</h1>")), "{path}");
        assert!(body.contains(&format!("mailto:{}", app::views::legal::CONTACT_EMAIL)), "{path}");
        assert!(home.contains(&format!(r#"href="{path}""#)), "{path}");
    }
    let (_, _, terms) = send(&app, get("/terms", None)).await;
    assert_eq!(terms.contains("billed in advance through Stripe"), cfg!(feature = "stripe"));
    let (_, _, privacy) = send(&app, get("/privacy", None)).await;
    assert_eq!(privacy.contains("<strong>Mailgun</strong>"), cfg!(feature = "auth"));
    assert_eq!(privacy.contains("<strong>Google</strong>"), cfg!(feature = "google"));
    assert_eq!(privacy.contains("<strong>GitHub</strong>"), cfg!(feature = "github"));
    assert_eq!(privacy.contains("<strong>Stripe</strong>"), cfg!(feature = "stripe"));
    assert_eq!(privacy.contains("<strong>PostHog</strong>"), cfg!(feature = "posthog"));
    assert_eq!(privacy.contains("<strong>Sentry</strong>"), cfg!(feature = "sentry"));
    assert_eq!(privacy.contains("file storage provider"), cfg!(feature = "storage"));
    assert_eq!(privacy.contains("session replays"), cfg!(feature = "sentry-replay"));
}

/// Search engines and AI assistants read these pages to check the business is real.
#[sqlx::test(migrations = "../migrations")]
async fn about_contact_and_privacy_pages_have_real_content(pool: PgPool) {
    let app = app(pool);
    let (_, _, home) = send(&app, get("/", None)).await;
    for (path, page_type) in [("/about", Some("AboutPage")), ("/contact", Some("ContactPage")), ("/privacy", None)] {
        let (status, _, body) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(&format!("mailto:{}", app::views::legal::CONTACT_EMAIL)), "{path}");
        assert!(body.contains("index, follow"), "{path}");
        assert!(home.contains(&format!(r#"href="{path}""#)), "{path}");
        if let Some(page_type) = page_type {
            assert!(body.contains(&format!(r#""@type":"{page_type}""#)), "{path}");
        }
        let main = body.split("<main").nth(1).unwrap().split("</main>").next().unwrap();
        let mut text = String::new();
        let mut in_tag = false;
        for c in main.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => text.push(c),
                _ => {}
            }
        }
        let chars = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().count();
        assert!(chars >= 500, "{path} has {chars} characters of text");
    }
    let (_, _, about) = send(&app, get("/about", None)).await;
    assert!(about.contains(app::views::company::FOUNDER));
    assert_eq!(about.contains("cancel any time from the billing page"), cfg!(feature = "stripe"));
    let (_, _, contact) = send(&app, get("/contact", None)).await;
    assert_eq!(contact.contains("Account help."), cfg!(feature = "auth"));
    assert_eq!(contact.contains("Billing and refunds."), cfg!(feature = "stripe"));
}

#[sqlx::test(migrations = "../migrations")]
async fn sitemap_normalizes_and_escapes_the_configured_url(pool: PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "https://example.com/a&b/".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let app = app::build(cfg, pool).1;
    let (status, _, body) = send(&app, get("/sitemap.xml", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<loc>https://example.com/a&amp;b/</loc>"), "{body}");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn dashboard_requires_sign_in(pool: PgPool) {
    let app = app(pool);
    let (status, headers, _) = send(&app, get("/dashboard", None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/sign-in");
}

#[cfg(not(feature = "auth"))]
#[sqlx::test(migrations = "../migrations")]
async fn without_auth_there_are_no_account_routes_or_links(pool: PgPool) {
    let app = app(pool);
    for path in ["/sign-up", "/sign-in", "/verify-email", "/dashboard", "/settings", "/settings/two-factor", "/admin"] {
        assert_eq!(send(&app, get(path, None)).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    for path in ["/sign-up", "/sign-in", "/sign-out", "/passkeys/sign-in/options"] {
        assert_eq!(send(&app, form(path, "", None)).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    let (status, _, body) = send(&app, get("/", None)).await;
    assert_eq!(status, StatusCode::OK);
    for link in ["href=\"/sign-up\"", "href=\"/sign-in\"", "href=\"/dashboard\""] {
        assert!(!body.contains(link), "{link}");
    }
    assert!(body.contains("Get in touch"));
    let (_, _, privacy) = send(&app, get("/privacy", None)).await;
    assert!(!privacy.contains("Mailgun") && !privacy.contains("session cookie"), "{privacy}");
}

/// Every sign-in provider the template offers: `(id, name, compiled into this build)`.
#[cfg(feature = "auth")]
const PROVIDERS: [(&str, &str, bool); 2] =
    [("google", "Google", cfg!(feature = "google")), ("github", "GitHub", cfg!(feature = "github"))];

/// Without credentials a provider is invisible, and its callback never caches or refers.
#[cfg(any(feature = "google", feature = "github"))]
#[sqlx::test(migrations = "../migrations")]
async fn sign_in_providers_are_hidden_without_configuration_and_callbacks_are_private(pool: PgPool) {
    let app = app(pool);
    for (id, _, _) in PROVIDERS.into_iter().filter(|p| p.2) {
        for path in ["/sign-in", "/sign-up"] {
            let (_, _, body) = send(&app, get(path, None)).await;
            assert!(!body.contains(&format!("action=\"/auth/{id}\"")), "{path}");
        }
        let callback = format!("/auth/{id}/callback?code=private-code&state=private-state");
        let (status, headers, _) = send(&app, get(&callback, None)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], format!("/sign-in?{id}=unavailable").as_str());
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        let (_, _, body) = send(&app, get(&format!("/sign-in?{id}=failed"), None)).await;
        assert!(body.contains("Please start again"));
        let (_, _, body) = send(&app, get(&format!("/sign-in?{id}=unknown"), None)).await;
        assert!(!body.contains("class=\"alert\""));
        // Provider forms require positive origin evidence like the other auth forms.
        let request = Request::post(format!("/auth/{id}")).body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
    }
}

/// Each provider feature owns its routes and settings panel; nothing leaks without it.
#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn disabled_sign_in_providers_have_no_routes_or_panels(pool: PgPool) {
    let (_, cookie) = settings_user(&pool, "no-provider@example.com").await;
    let app = app(pool);
    for (id, name, _) in PROVIDERS.into_iter().filter(|p| !p.2) {
        let (status, _, _) = send(&app, get(&format!("/auth/{id}/callback?code=x&state=y"), None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = send(&app, form(&format!("/auth/{id}"), "", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) =
            send(&app, form(&format!("/settings/{id}"), "action=link&password=x", Some(&cookie))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
        assert!(!settings.contains(&format!("{name} sign-in")) && !settings.contains(&format!("/settings/{id}")));
        let (_, _, sign_in) = send(&app, get(&format!("/sign-in?{id}=failed"), None)).await;
        assert!(!sign_in.contains(&format!("/auth/{id}")) && !sign_in.contains("Please start again"));
    }
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn sign_up_sign_out_sign_in(pool: PgPool) {
    let app = app(pool);

    // Signup is pending until the emailed code is verified.
    let (status, headers, _) =
        send(&app, form("/sign-up", "email=Ann%40Example.com&password=correct-horse", None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/verify-email");
    let verified = common::complete(&app, &session_cookie(&headers), "ann@example.com").await;
    assert_eq!(verified.status(), StatusCode::SEE_OTHER);
    let cookie = session_cookie(verified.headers());

    let (status, _, body) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ann@example.com"), "email is normalised: {body}");

    // Signed-in users are bounced away from the auth pages.
    let (status, headers, _) = send(&app, get("/sign-in", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/dashboard");

    // Sign out revokes the session server-side, not just the cookie.
    let (status, headers, _) = send(&app, form("/sign-out", "", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(headers[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let (status, _, _) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Sign in with the right password works, with the wrong one does not.
    let (status, _, body) = send(&app, form("/sign-in", "email=ann%40example.com&password=wrong-horse", None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Wrong email or password"));
    let (status, headers, _) =
        send(&app, form("/sign-in", "email=ann%40example.com&password=correct-horse", None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_cookie(&headers);
    let (status, _, _) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn sign_up_validation(pool: PgPool) {
    let app = app(pool);
    let cases = [("email=bad&password=correct-horse", "valid email"), ("email=a%40b.co&password=short", "at least 8")];
    for (body, expect) in cases {
        let (status, _, html) = send(&app, form("/sign-up", body, None)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(html.contains(expect), "{body}: {html}");
    }
    send(&app, form("/sign-up", "email=dup%40example.com&password=correct-horse", None)).await;
    let (status, _, html) = send(&app, form("/sign-up", "email=DUP%40example.com&password=correct-horse", None)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(html.contains("Please wait"));
}

#[sqlx::test(migrations = "../migrations")]
async fn cross_site_posts_are_blocked(pool: PgPool) {
    let app = app(pool);
    let req = Request::post("/sign-in")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from("email=a%40b.co&password=correct-horse"))
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let req = Request::post("/sign-in")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, "http://evil.local")
        .body(Body::from("email=a%40b.co&password=correct-horse"))
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../migrations")]
async fn every_response_carries_the_build_revision(pool: PgPool) {
    let app = app(pool);
    for path in ["/", "/healthz", "/nope"] {
        let (_, headers, _) = send(&app, get(path, None)).await;
        assert_eq!(headers["x-deployment-revision"], app_core::http::BUILD_REVISION, "{path}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn assets_are_immutable(pool: PgPool) {
    let app = app(pool);
    let (status, headers, _) = send(&app, get("/assets/style.css?v=abc", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/css"));

    let (status, headers, _) = send(&app, get("/assets/favicon.svg", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/svg+xml");
    let (status, _, _) = send(&app, get("/assets/missing.css", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The binary applies `app::MIGRATOR` at startup; it must carry this
/// project's full schema.
#[sqlx::test(migrations = false)]
async fn embedded_migrator_is_this_projects_schema(pool: PgPool) {
    app::MIGRATOR.run(&pool).await.unwrap();
    let latest = app::MIGRATOR.iter().map(|m| m.version).max();
    assert_eq!(latest, Some(16), "expected migrations 0001..0016");
    sqlx::query("SELECT id FROM sessions LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT id, redirect_uris FROM oauth_clients LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT code_hash, code_challenge FROM oauth_codes LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT access_hash, refresh_hash FROM oauth_tokens LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT user_id, provider, subject FROM social_identities LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT passwordless FROM users LIMIT 0").execute(&pool).await.unwrap();
    sqlx::query("SELECT user_id, secret, confirmed_at, last_used_step FROM totp_secrets LIMIT 0")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("SELECT id, kind, payload, dedupe_key, locked_until FROM jobs LIMIT 0").execute(&pool).await.unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn missing_resources_negotiate_markdown(pool: PgPool) {
    let app = app(pool);
    for path in ["/some-path-that-does-not-exist", "/assets/missing.css"] {
        let request = Request::get(path).header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap();
        let (status, headers, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], "text/markdown; charset=utf-8");
        assert_eq!(headers[header::VARY], "Accept");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert!(body.starts_with("# 404 — Not found\n\n"));
        assert!(body.contains("The requested page or resource could not be found."));
        assert!(body.contains("[Sitemap](/sitemap.xml)"));
        assert!(!body.contains("<html"));
    }
    let (status, _, _) = send(&app, get("/sitemap.xml", None)).await;
    assert_eq!(status, StatusCode::OK, "the recovery link must work");

    for accept in [None, Some("text/html"), Some("*/*"), Some("text/markdown;q=0")] {
        let mut request = get("/missing", None);
        if let Some(accept) = accept {
            request.headers_mut().insert(header::ACCEPT, accept.parse().unwrap());
        }
        let (status, headers, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(headers[header::VARY], "Accept");
        assert!(body.contains("<h1>Page not found</h1>"));
    }

    let request = Request::head("/missing").header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap();
    let (status, headers, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[header::CONTENT_TYPE], "text/markdown; charset=utf-8");
    assert!(body.is_empty());

    let request = Request::get("/").header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap();
    let (status, headers, _) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/markdown; charset=utf-8");
}

#[cfg(feature = "blog")]
#[sqlx::test(migrations = "../migrations")]
async fn blog_renders_articles_metadata_and_rss(pool: PgPool) {
    let app = app(pool);
    let (status, _, index) = send(&app, get("/blog", None)).await;
    assert_eq!(status, StatusCode::OK);
    let posts = app::blog::Blog::load("http://test.local").unwrap().posts;
    let (status, headers, rss) = send(&app, get("/blog/rss.xml", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/rss+xml; charset=utf-8");
    for post in posts {
        assert!(index.contains(&format!("href=\"{}\"", post.path)));
        let (status, headers, article) = send(&app, get(&post.path, None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert!(article.contains(&format!("rel=\"canonical\" href=\"http://test.local{}\"", post.path)));
        assert!(article.contains("property=\"og:type\" content=\"article\""));
        assert!(article.contains(&post.body.into_string()));
        assert!(rss.contains(&format!("<link>http://test.local{}</link>", post.path)));
    }
    for accept in ["text/html", "text/markdown"] {
        let req = Request::get("/blog/missing").header(header::ACCEPT, accept).body(Body::empty()).unwrap();
        let (status, headers, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with(accept));
    }
}

#[cfg(not(feature = "blog"))]
#[sqlx::test(migrations = "../migrations")]
async fn disabled_blog_has_no_routes_navigation_or_sitemap_entries(pool: PgPool) {
    let app = app(pool);
    for path in ["/blog", "/blog/welcome", "/blog/rss.xml"] {
        let (status, _, _) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    for path in ["/", "/terms", "/sitemap.xml"] {
        let (status, _, body) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("href=\"/blog"), "{path}: {body}");
        assert!(!body.contains("<loc>http://test.local/blog"), "{path}: {body}");
    }
}

#[cfg(feature = "docs")]
#[sqlx::test(migrations = "../migrations")]
async fn docs_render_navigation_metadata_and_not_found(pool: PgPool) {
    let app = app(pool);
    let (status, headers, index) = send(&app, get("/docs", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let docs = app::docs::Docs::load().unwrap();
    assert_eq!(docs.welcome.path, "/docs");
    assert_eq!(docs.pages[0].slug, "getting-started");
    assert!(index.contains("<h1>Welcome to LVTD</h1>"));
    assert!(index.contains("<h2>What is LVTD?</h2>"));
    assert!(index.contains("rel=\"canonical\" href=\"http://test.local/docs\""));
    assert!(index.contains(&format!("name=\"description\" content=\"{}\"", docs.welcome.description)));
    assert!(!index.contains("href=\"/docs/welcome\""));
    assert!(!index.contains("docs-card"));
    let sections: Vec<_> =
        index.split("class=\"docs-nav-heading\">").skip(1).map(|part| part.split("</h2>").next().unwrap()).collect();
    assert_eq!(sections, ["Getting started", "Features", "Deployment", "Developer tools"]);
    assert_eq!(index.matches("aria-current=\"page\"").count(), 1);
    for accept in ["text/html", "text/markdown"] {
        let req = Request::get("/docs/welcome").header(header::ACCEPT, accept).body(Body::empty()).unwrap();
        let (status, headers, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(headers[header::LOCATION], "/docs");
    }
    let (_, _, getting_started) = send(&app, get("/docs/getting-started", None)).await;
    for path in ["/sign-up", "/sign-in", "/dashboard"] {
        assert!(getting_started.contains(&format!("href=\"{path}\"")));
        // The guide describes accounts either way; the routes exist only with `auth`.
        assert_eq!(send(&app, get(path, None)).await.0 != StatusCode::NOT_FOUND, cfg!(feature = "auth"), "{path}");
    }

    let (_, _, sitemap) = send(&app, get("/sitemap.xml", None)).await;
    assert!(!sitemap.contains("/docs/welcome"));
    assert_eq!(sitemap.matches("<loc>http://test.local/docs</loc>").count(), 1);
    for doc in docs.pages {
        assert!(index.contains(&format!("href=\"{}\"", doc.path)));
        assert!(sitemap.contains(&format!("<loc>http://test.local{}</loc>", doc.path)));
        let (status, _, page) = send(&app, get(&doc.path, None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(&format!("<h1>{}</h1>", doc.title)));
        assert!(page.contains(&format!("rel=\"canonical\" href=\"http://test.local{}\"", doc.path)));
        assert!(page.contains(&format!("name=\"description\" content=\"{}\"", doc.description)));
        assert_eq!(page.matches("aria-current=\"page\"").count(), 1);
        assert!(page.contains(&format!("href=\"{}\" aria-current=\"page\"", doc.path)));
        assert!(page.contains("<h2>"));
    }
    for accept in ["text/html", "text/markdown"] {
        let req = Request::get("/docs/missing").header(header::ACCEPT, accept).body(Body::empty()).unwrap();
        let (status, headers, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with(accept));
    }
    let (_, _, home) = send(&app, get("/", None)).await;
    assert!(home.contains("href=\"/docs\""));
}

#[cfg(feature = "docs")]
#[sqlx::test(migrations = "../migrations")]
async fn llms_index_links_to_full_documentation(pool: PgPool) {
    let app = app(pool);
    let (status, headers, llms) = send(&app, get("/llms.txt", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    let docs = app::docs::Docs::load().unwrap();
    let header = format!("# {}\n\n> {}\n", app::views::SITE_NAME, docs.welcome.description);
    assert!(llms.starts_with(&header));
    assert!(llms.chars().count() < 30_000);
    assert!(llms.contains("[Full documentation](http://test.local/llms-full.txt)"));
    assert!(!llms.contains("Source: "));
    let (status, headers, full) = send(&app, get("/llms-full.txt", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    for doc in std::iter::once(&docs.welcome).chain(&docs.pages) {
        assert!(llms.contains(&format!("- [{}](http://test.local{}): {}\n", doc.title, doc.path, doc.description)));
        assert!(full.contains(&format!("\n### {}\n\nSource: http://test.local{}\n", doc.title, doc.path)));
    }
    assert!(!llms.contains("\n#### What is LVTD?\n"));
    assert!(full.contains("\n#### What is LVTD?\n"));
}

#[cfg(not(feature = "docs"))]
#[sqlx::test(migrations = "../migrations")]
async fn disabled_docs_has_no_routes_navigation_or_sitemap_entries(pool: PgPool) {
    let app = app(pool);
    for path in [
        "/llms.txt",
        "/llms-full.txt",
        "/docs",
        "/docs/welcome",
        "/docs/getting-started",
        "/docs/blog",
        "/docs/writing-docs",
        "/docs/sentry",
        "/docs/posthog",
        "/docs/docker",
        "/docs/caprover",
    ] {
        let (status, _, _) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    for path in ["/", "/sitemap.xml"] {
        let (_, _, body) = send(&app, get(path, None)).await;
        assert!(!body.contains("/docs"));
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn browser_telemetry_obeys_compiled_choices_and_runtime_token(pool: PgPool) {
    let app = app(pool.clone());
    let (_, _, body) = send(&app, get("/", None)).await;
    assert!(!body.contains("analytics.js"));
    #[cfg(feature = "posthog")]
    {
        let cfg = Config {
            bind_addr: "127.0.0.1:0".into(),
            site_url: "http://test.local".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        };
        let analytics = app::analytics::Config::parse("phc_test\"><script>", "https://eu.i.posthog.com").unwrap();
        let app = app::build_with_analytics(cfg, pool, analytics).1;
        let (_, _, body) = send(&app, get("/", None)).await;
        let browser = cfg!(feature = "posthog-web");
        assert_eq!(body.contains("analytics.js"), browser);
        if browser {
            assert!(!body.contains("phc_test\"><script>"));
            assert!(body.contains("https://eu-assets.i.posthog.com/static/array.js"));
            assert!(body.contains(&format!("data-web-analytics=\"{}\"", cfg!(feature = "posthog-web"))));
            assert!(!body.contains("data-error-tracking"));
        }
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn stripe_routes_are_absent_when_disabled_or_unavailable_without_config(pool: PgPool) {
    let app = app(pool);
    let (code, _, _) = send(&app, get("/billing", None)).await;
    assert_eq!(code, if cfg!(feature = "stripe") { StatusCode::SEE_OTHER } else { StatusCode::NOT_FOUND });
    let req = Request::post("/stripe/webhook").body(Body::from("{}")).unwrap();
    let (code, _, _) = send(&app, req).await;
    assert_eq!(code, if cfg!(feature = "stripe") { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::NOT_FOUND });
}

#[cfg(feature = "auth")]
async fn settings_user(pool: &PgPool, email: &str) -> (app_auth::User, String) {
    let user = app_auth::users::sign_up(pool, email, "correct-horse".into()).await.unwrap();
    let token = app_auth::sessions::create(pool, user.id, time::Duration::days(1)).await.unwrap();
    (user, format!("session={token}"))
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_table_of_contents_links_every_section(pool: PgPool) {
    let (_, cookie) = settings_user(&pool, "toc@example.com").await;
    let (status, _, page) = send(&app(pool), get("/settings", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, rest) = page.split_once("class=\"settings-toc\"").expect("settings has a table of contents");
    let (toc, sections) = rest.split_once("</nav>").unwrap();
    let sections = sections.split_once("</main>").unwrap().0;
    let anchors: Vec<&str> = toc.split("href=\"#").skip(1).map(|s| s.split('"').next().unwrap()).collect();
    assert!(anchors.len() >= 5 && anchors.contains(&"profile") && anchors.contains(&"delete-account"), "{toc}");
    for anchor in &anchors {
        assert!(sections.contains(&format!("id=\"{anchor}\"")), "#{anchor} has no section");
    }
    for label in sections.split("aria-labelledby=\"").skip(1).map(|s| s.split('"').next().unwrap()) {
        assert!(sections.contains(&format!("<h3 id=\"{label}\">")), "panel heading {label} is missing");
    }
    // Every panel is listed, so nothing on the page is unreachable from the contents.
    assert_eq!(sections.matches("<section class=\"panel").count(), anchors.len(), "{sections}");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_require_authentication_and_reject_cross_origin_changes(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());
    for req in
        [get("/settings", None), form("/settings", "action=delete&confirmation=DELETE&password=correct-horse", None)]
    {
        let (status, headers, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[header::LOCATION], "/sign-in");
    }
    for action in [
        "profile&name=Changed&email=owner@example.com",
        "delete&confirmation=DELETE&password=correct-horse",
        "portal",
        "remove_avatar",
    ] {
        let mut request = form("/settings", &format!("action={action}"), Some(&cookie));
        request.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert_eq!(send(&app, request).await.0, StatusCode::FORBIDDEN);
    }
    // The multipart upload route is guarded the same way (and absent without the feature).
    let upload = |cookie| common::multipart("/settings/avatar", "image", "me.png", "image/png", PNG, cookie);
    let (status, headers, _) = send(&app, upload(None)).await;
    if cfg!(feature = "storage") {
        assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in"));
        let mut request = upload(Some(&cookie));
        request.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert_eq!(send(&app, request).await.0, StatusCode::FORBIDDEN);
    } else {
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    assert_eq!(app::settings::profile(&pool, user.id).await.unwrap().name, "");
    assert_eq!(avatar_key(&pool, user.id).await, None);
}

/// Enough of a PNG to be recognized; nothing decodes it.
#[cfg(feature = "auth")]
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR-test-image";
#[cfg(feature = "storage")]
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00-test-image";

#[cfg(feature = "auth")]
async fn avatar_key(pool: &PgPool, user_id: uuid::Uuid) -> Option<String> {
    sqlx::query_scalar!("SELECT avatar_key FROM users WHERE id = $1", user_id).fetch_one(pool).await.unwrap()
}

#[cfg(feature = "storage")]
#[sqlx::test(migrations = "../migrations")]
async fn profile_pictures_upload_serve_replace_and_remove(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "picture@example.com").await;
    let (other, other_cookie) = settings_user(&pool, "other@example.com").await;
    let app = app(pool.clone());
    let (_, _, page) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(page.contains("Profile picture") && page.contains("enctype=\"multipart/form-data\""), "{page}");
    assert!(page.contains("action=\"/settings/avatar\"") && page.contains("name=\"image\""));
    assert!(page.contains("avatar-placeholder") && !page.contains("remove_avatar"));
    let (_, _, dashboard) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert!(dashboard.contains("<span class=\"account-avatar\" aria-hidden=\"true\">P</span>"));

    // Rejected uploads re-render the settings page with the reason; the bytes decide, not the name.
    let upload = |field: &str, name: &str, content_type: &str, data: &[u8]| {
        common::multipart("/settings/avatar", field, name, content_type, data, Some(&cookie))
    };
    let mut huge = PNG.to_vec();
    huge.resize(app::avatars::MAX_BYTES + 1, 0);
    for (field, name, content_type, data, message) in [
        ("image", "x.svg", "image/svg+xml", &b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"[..], "PNG, JPEG"),
        ("image", "x.png", "image/png", b"not a png at all", "PNG, JPEG"),
        ("image", "x.png", "image/png", b"", "PNG, JPEG"),
        ("photo", "x.png", "image/png", PNG, "PNG, JPEG"),
        ("image", "x.png", "image/png", &huge, "too large"),
    ] {
        let (status, _, body) = send(&app, upload(field, name, content_type, data)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name} {message}");
        assert!(body.contains(message) && body.contains("Account settings"), "{name}: {body}");
        assert_eq!(avatar_key(&pool, user.id).await, None);
    }

    let (status, headers, _) = send(&app, upload("image", "me.PNG", "application/octet-stream", PNG)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings?saved=avatar");
    let key = avatar_key(&pool, user.id).await.unwrap();
    assert!(key.starts_with(&format!("avatars/{}/", user.id)) && key.ends_with(".png"), "{key}");
    assert_eq!(avatar_key(&pool, other.id).await, None);
    let (_, _, page) = send(&app, get("/settings?saved=avatar", Some(&cookie))).await;
    assert!(page.contains("Your profile picture has been updated"));
    assert!(page.contains(&format!("<img class=\"avatar-preview\" src=\"/{key}\"")) && page.contains("remove_avatar"));
    let (_, _, dashboard) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert!(dashboard.contains(&format!("<img class=\"account-avatar\" src=\"/{key}\" alt=\"\"")), "{dashboard}");
    let (_, _, other_dashboard) = send(&app, get("/dashboard", Some(&other_cookie))).await;
    assert!(!other_dashboard.contains("<img class=\"account-avatar\""));

    // Pictures are public, served with the sniffed type and cached for good.
    let response = app.clone().oneshot(get(&format!("/{key}"), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(&response.into_body().collect().await.unwrap().to_bytes()[..], PNG);
    for path in [
        format!("/avatars/{}/missing.png", user.id),
        "/avatars/../Cargo.toml".to_owned(),
        format!("/avatars/{}/..%2F..%2FCargo.toml", user.id),
        "/avatars/".to_owned(),
    ] {
        assert_eq!(send(&app, get(&path, None)).await.0, StatusCode::NOT_FOUND, "{path}");
    }

    // Replacing deletes the old object; the URL changes so caches never go stale.
    let (status, _, _) = send(&app, upload("image", "me.gif", "image/gif", GIF)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let replaced = avatar_key(&pool, user.id).await.unwrap();
    assert_ne!(replaced, key);
    assert!(replaced.ends_with(".gif"));
    assert_eq!(send(&app, get(&format!("/{key}"), None)).await.0, StatusCode::NOT_FOUND);
    let (status, headers, _) = send(&app, get(&format!("/{replaced}"), None)).await;
    assert_eq!((status, headers[header::CONTENT_TYPE].to_str().unwrap()), (StatusCode::OK, "image/gif"));

    let (status, headers, _) = send(&app, form("/settings", "action=remove_avatar", Some(&cookie))).await;
    assert_eq!(
        (status, headers[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/settings?saved=avatar-removed")
    );
    assert_eq!(avatar_key(&pool, user.id).await, None);
    assert_eq!(send(&app, get(&format!("/{replaced}"), None)).await.0, StatusCode::NOT_FOUND);
    let (_, _, page) = send(&app, get("/settings?saved=avatar-removed", Some(&cookie))).await;
    assert!(page.contains("Your profile picture has been removed") && page.contains("avatar-placeholder"));
    assert!(!page.contains("remove_avatar"));
    assert_eq!(send(&app, form("/settings", "action=remove_avatar", Some(&cookie))).await.0, StatusCode::SEE_OTHER);
}

#[cfg(feature = "storage")]
#[sqlx::test(migrations = "../migrations")]
async fn profile_pictures_use_the_bucket_and_its_public_url_when_configured(pool: PgPool) {
    let (endpoint, objects) = common::s3_double().await;
    let storage = app_core::storage::Storage::s3(app_core::storage::S3Config {
        bucket: "uploads".into(),
        endpoint,
        region: "auto".into(),
        access_key_id: common::S3_KEY_ID.into(),
        secret_access_key: "test-secret".into(),
        public_url: Some("https://files.example.com".into()),
    })
    .unwrap();
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let app = common::mail_app(app::build_configured(
        cfg,
        pool.clone(),
        #[cfg(feature = "posthog")]
        None,
        #[cfg(feature = "sentry")]
        None,
        #[cfg(feature = "stripe")]
        None,
        storage,
        #[cfg(feature = "qdrant")]
        None,
    ))
    .1;
    let (user, cookie) = settings_user(&pool, "bucket@example.com").await;
    let upload =
        |name: &str, data: &[u8]| common::multipart("/settings/avatar", "image", name, "image/*", data, Some(&cookie));
    let (status, _, _) = send(&app, upload("me.png", PNG)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let key = avatar_key(&pool, user.id).await.unwrap();
    assert_eq!(objects.lock().unwrap().get(&format!("uploads/{key}")), Some(&("image/png".to_owned(), PNG.to_vec())));
    // Linked straight from the public URL, yet still served by the app when asked.
    let (_, _, page) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(page.contains(&format!("src=\"https://files.example.com/{key}\"")), "{page}");
    let (_, _, dashboard) = send(&app, get("/dashboard", Some(&cookie))).await;
    assert!(dashboard.contains(&format!("<img class=\"account-avatar\" src=\"https://files.example.com/{key}\"")));
    let response = app.clone().oneshot(get(&format!("/{key}"), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(&response.into_body().collect().await.unwrap().to_bytes()[..], PNG);
    assert_eq!(send(&app, get(&format!("/avatars/{}/nope.png", user.id), None)).await.0, StatusCode::NOT_FOUND);

    let (status, _, _) = send(&app, upload("me.gif", GIF)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let replaced = avatar_key(&pool, user.id).await.unwrap();
    assert_eq!(objects.lock().unwrap().keys().collect::<Vec<_>>(), [&format!("uploads/{replaced}")]);
    assert_eq!(send(&app, form("/settings", "action=remove_avatar", Some(&cookie))).await.0, StatusCode::SEE_OTHER);
    assert!(objects.lock().unwrap().is_empty());
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_save_and_clear_name_without_changing_another_account(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let (other, _) = settings_user(&pool, "other@example.com").await;
    let app = app(pool.clone());
    let (status, headers, body) = send(&app, get("/settings", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(body.contains("Manage billing"), cfg!(feature = "stripe"));
    assert!(!body.contains("$999") && !body.contains("unlimited projects"));
    assert!(send(&app, get("/dashboard", Some(&cookie))).await.2.contains("href=\"/settings\""));
    let (status, headers, _) = send(
        &app,
        form(
            "/settings",
            &format!("action=profile&name=++Ada+Lovelace++&email=owner@example.com&user_id={}", other.id),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings?saved=1");
    assert_eq!(app::settings::profile(&pool, user.id).await.unwrap().name, "Ada Lovelace");
    assert_eq!(app::settings::profile(&pool, other.id).await.unwrap().name, "");
    assert!(send(&app, get("/settings?saved=1", Some(&cookie))).await.2.contains("Your profile has been saved"));
    assert_eq!(
        send(&app, form("/settings", "action=profile&name=&email=owner@example.com", Some(&cookie))).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(app::settings::profile(&pool, user.id).await.unwrap().name, "");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_email_changes_require_password_and_revoke_other_sessions(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let other_token = app_auth::sessions::create(&pool, user.id, time::Duration::days(1)).await.unwrap();
    let app = app(pool.clone());
    for password in ["", "wrong-password"] {
        let (status, _, body) = send(
            &app,
            form(
                "/settings",
                &format!("action=profile&name=New+Name&email=NEW@example.com&password={password}"),
                Some(&cookie),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("value=\"New Name\"") && body.contains("value=\"NEW@example.com\""));
        assert!(!body.contains("value=\"wrong-password\""));
        assert_eq!(app::settings::profile(&pool, user.id).await.unwrap().email, "owner@example.com");
    }
    assert_eq!(
        send(
            &app,
            form(
                "/settings",
                "action=profile&name=New+Name&email=++NEW@example.com++&password=correct-horse",
                Some(&cookie)
            )
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let profile = app::settings::profile(&pool, user.id).await.unwrap();
    assert_eq!(profile.email, "new@example.com");
    assert_eq!(profile.name, "New Name");
    assert!(app_auth::sessions::lookup(&pool, &other_token).await.unwrap().is_none());
    assert_eq!(send(&app, get("/settings", Some(&cookie))).await.0, StatusCode::OK);
    assert!(
        app_auth::users::verify_credentials(&pool, "owner@example.com", "correct-horse".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        app_auth::users::verify_credentials(&pool, "new@example.com", "correct-horse".into())
            .await
            .unwrap()
            .unwrap()
            .id,
        user.id
    );
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_reject_duplicate_and_invalid_emails_and_escape_names(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    settings_user(&pool, "taken@example.com").await;
    let app = app(pool.clone());
    for email in ["taken@example.com", "invalid", "a@@example.com", "a+b@example.com"] {
        let (status, _, body) = send(
            &app,
            form(
                "/settings",
                &format!("action=profile&name=%3Cscript%3E&email={email}&password=correct-horse"),
                Some(&cookie),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{email}");
        assert!(body.contains("&lt;script&gt;"));
        assert!(!body.contains("value=\"<script>\""));
    }
    for name in ["a".repeat(101), "bad%00name".into()] {
        assert_eq!(
            send(
                &app,
                form("/settings", &format!("action=profile&name={name}&email=owner@example.com"), Some(&cookie))
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let profile = app::settings::profile(&pool, user.id).await.unwrap();
    assert_eq!(profile.email, "owner@example.com");
    assert_eq!(profile.name, "");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_deletion_requires_confirmation_and_password_then_cascades(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let (other, other_cookie) = settings_user(&pool, "other@example.com").await;
    sqlx::query!("INSERT INTO billing_accounts (user_id, paid_at) VALUES ($1, now())", user.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query!("INSERT INTO billing_checkouts (id, user_id, mode, price_id, active, email) VALUES ($1, $2, 'payment', 'price_test', false, 'owner@example.com')", uuid::Uuid::now_v7(), user.id).execute(&pool).await.unwrap();
    let app = app(pool.clone());
    #[cfg(feature = "storage")]
    let picture = {
        let (status, _, _) =
            send(&app, common::multipart("/settings/avatar", "image", "me.png", "image/png", PNG, Some(&cookie))).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let picture = format!("/{}", avatar_key(&pool, user.id).await.unwrap());
        assert_eq!(send(&app, get(&picture, None)).await.0, StatusCode::OK);
        picture
    };
    for body in
        ["action=delete&confirmation=wrong&password=correct-horse", "action=delete&confirmation=DELETE&password=wrong"]
    {
        assert_eq!(send(&app, form("/settings", body, Some(&cookie))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(app::settings::profile(&pool, user.id).await.is_ok());
    }
    let (_, _, failure) =
        send(&app, form("/settings", "action=delete&confirmation=WRONG&password=correct-horse", Some(&cookie))).await;
    assert!(failure.contains("<details open>") && failure.contains("value=\"WRONG\""));
    assert!(!failure.contains("value=\"correct-horse\""));
    let (status, headers, _) = send(
        &app,
        form(
            "/settings",
            &format!("action=delete&confirmation=DELETE&password=correct-horse&user_id={}", other.id),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
    assert!(headers[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    assert!(app::settings::profile(&pool, user.id).await.is_err());
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"count!\" FROM billing_checkouts WHERE user_id = $1", user.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count,
        0
    );
    assert_eq!(send(&app, get("/settings", Some(&cookie))).await.1[header::LOCATION], "/sign-in");
    assert_eq!(send(&app, get("/settings", Some(&other_cookie))).await.0, StatusCode::OK);
    // The stored picture went with the account.
    #[cfg(feature = "storage")]
    assert_eq!(send(&app, get(&picture, None)).await.0, StatusCode::NOT_FOUND);
}

// Billing tables persist even if the optional Stripe feature is later disabled.
#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn settings_deletion_blocks_pending_checkouts_and_nonterminal_subscriptions(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());
    sqlx::query!("INSERT INTO billing_accounts (user_id) VALUES ($1)", user.id).execute(&pool).await.unwrap();
    let attempt = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO billing_checkouts (id, user_id, mode, price_id, email) VALUES ($1, $2, 'payment', 'price_test', 'owner@example.com')",
        attempt,
        user.id
    )
    .execute(&pool)
    .await
    .unwrap();
    let deletion = "action=delete&confirmation=DELETE&password=correct-horse";
    let (status, _, body) = send(&app, form("/settings", deletion, Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Resolve pending payments"));
    assert!(app::settings::profile(&pool, user.id).await.is_ok());
    sqlx::query!("UPDATE billing_checkouts SET active = false WHERE id = $1", attempt).execute(&pool).await.unwrap();
    for status in ["active", "trialing", "past_due", "unpaid", "paused", "incomplete"] {
        sqlx::query!("INSERT INTO billing_subscriptions (id, user_id, status, cancel_at_period_end) VALUES ('sub_settings', $1, $2, true) ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status", user.id, status).execute(&pool).await.unwrap();
        assert_eq!(
            send(&app, form("/settings", deletion, Some(&cookie))).await.0,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{status}"
        );
    }
    sqlx::query!("UPDATE billing_subscriptions SET status = 'canceled' WHERE user_id = $1", user.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(send(&app, form("/settings", deletion, Some(&cookie))).await.0, StatusCode::SEE_OTHER);
    assert!(app::settings::profile(&pool, user.id).await.is_err());
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn account_navigation_and_appearance_match_enabled_features(pool: PgPool) {
    let (_, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool);
    let (_, _, dashboard) = send(&app, get("/dashboard", Some(&cookie))).await;
    let header = dashboard.split("<header").nth(1).unwrap().split("</header>").next().unwrap();
    assert!(header.contains("href=\"/dashboard\" aria-current=\"page\""));
    assert!(header.contains("Account menu") && header.contains("owner@example.com"));
    assert!(header.contains("href=\"/settings\"") && header.contains("action=\"/sign-out\""));
    assert!(!header.contains("theme-toggle") && !header.contains("Projects"));
    assert_eq!(header.contains("href=\"/docs\""), cfg!(feature = "docs"));
    assert_eq!(header.contains("href=\"/blog\""), cfg!(feature = "blog"));
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("href=\"/settings\" aria-current=\"page\""));
    for choice in ["light", "dark", "system"] {
        assert!(settings.contains(&format!("data-theme-value=\"{choice}\"")));
    }
    assert_eq!(settings.contains("href=\"/billing\""), cfg!(feature = "stripe"));
    assert_eq!(settings.contains("Profile picture"), cfg!(feature = "storage"));
    assert_eq!(settings.contains("action=\"/settings/avatar\""), cfg!(feature = "storage"));
    assert!(!settings.contains("$999") && !settings.contains("unlimited projects"));
    let (_, _, public) = send(&app, get("/", None)).await;
    assert!(public.contains("href=\"/sign-in\"") && public.contains("href=\"/sign-up\""));
    assert!(!public.contains("<details class=\"account-menu\""));
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = false)]
async fn settings_migrations_preserve_existing_accounts_sessions_and_checkout_email(pool: PgPool) {
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Borrowed(&app::MIGRATOR.migrations[..3]),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    // Seed the old schema directly: today's sign_up query also returns is_admin.
    let user = sqlx::query!(
        "INSERT INTO users (id, email, password_hash) VALUES ($1, 'legacy@example.com', $2) RETURNING id",
        uuid::Uuid::now_v7(),
        app_auth::password::hash("correct-horse".into()).await.unwrap()
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let token = app_auth::sessions::create(&pool, user.id, time::Duration::days(1)).await.unwrap();
    sqlx::query!("INSERT INTO billing_accounts (user_id) VALUES ($1)", user.id).execute(&pool).await.unwrap();
    let attempt = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO billing_checkouts (id, user_id, mode, price_id) VALUES ($1, $2, 'payment', 'price_test')",
        attempt,
        user.id
    )
    .execute(&pool)
    .await
    .unwrap();
    app::MIGRATOR.run(&pool).await.unwrap();
    let profile = app::settings::profile(&pool, user.id).await.unwrap();
    assert_eq!(profile.name, "");
    assert_eq!(profile.email, "legacy@example.com");
    let session = app_auth::sessions::lookup(&pool, &token).await.unwrap().expect("preserved session");
    assert!(!session.user.is_admin, "upgrading an existing account must not grant admin access");
    let checkout = sqlx::query!("SELECT email, active FROM billing_checkouts WHERE id = $1", attempt)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(checkout.email, "legacy@example.com");
    assert!(checkout.active);
}

#[sqlx::test(migrations = "../migrations")]
async fn indexnow_matches_only_public_sitemap_pages(pool: PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, router) = app::build(cfg, pool);
    let paths = app::indexnow_paths(&state);
    let (_, _, sitemap) = send(&router, get("/sitemap.xml", None)).await;
    let mut sitemap_paths: Vec<_> = sitemap
        .split("<loc>")
        .skip(1)
        .map(|entry| entry.split("</loc>").next().unwrap().strip_prefix("http://test.local").unwrap().to_string())
        .collect();
    sitemap_paths.sort();
    sitemap_paths.dedup();
    assert_eq!(paths, sitemap_paths);
    for path in &paths {
        assert!(
            path == "/"
                || path == "/docs"
                || path.starts_with("/docs/")
                || path == "/blog"
                || path.starts_with("/blog/")
                || app::views::company::PATHS.contains(&path.as_str())
                || app::views::legal::PATHS.contains(&path.as_str())
        );
        assert_eq!(send(&router, get(path, None)).await.0, StatusCode::OK);
    }
}

// ---------------------------------------------------------------------------
// Passkeys
// ---------------------------------------------------------------------------

#[cfg(feature = "auth")]
fn json_post(path: &str, body: &serde_json::Value, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::post(path)
        .header(header::ORIGIN, "http://test.local")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[cfg(feature = "auth")]
async fn passkey_options(app: &Router, path: &str, body: &str, cookie: Option<&str>) -> serde_json::Value {
    let (status, headers, body) = send(app, form(path, body, cookie)).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    serde_json::from_str(&body).unwrap()
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn passkeys_are_added_in_settings_and_sign_users_in(pool: PgPool) {
    use app_auth::testing::{Algorithm, Authenticator};
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());

    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("data-passkey-register"), "{settings}");
    assert!(settings.contains("No passkeys yet"));
    assert!(settings.contains("<script defer src=\"/assets/passkeys.js?v="));
    let (_, _, sign_in) = send(&app, get("/sign-in", None)).await;
    assert!(sign_in.contains("data-passkey-signin") && sign_in.contains("autocomplete=\"username webauthn\""));
    assert!(sign_in.contains("/assets/passkeys.js?v="));
    let (_, _, sign_up) = send(&app, get("/sign-up", None)).await;
    assert!(!sign_up.contains("data-passkey-signin") && !sign_up.contains("passkeys.js"));

    // Registration needs a session and the current password.
    let (status, headers, _) = send(&app, form("/passkeys/register/options", "password=correct-horse", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in"));
    let (status, _, body) = send(&app, form("/passkeys/register/options", "password=wrong-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("current password"), "{body}");

    let options = passkey_options(&app, "/passkeys/register/options", "password=correct-horse", Some(&cookie)).await;
    assert_eq!(options["publicKey"]["rp"], serde_json::json!({ "id": "test.local", "name": SITE_NAME }));
    assert_eq!(options["publicKey"]["user"]["name"], "owner@example.com");
    assert_eq!(options["publicKey"]["authenticatorSelection"]["residentKey"], "required");
    let mut authenticator = Authenticator::new(Algorithm::Es256);
    let registration = authenticator.create_json("http://test.local", &options, "Laptop");
    let (status, _, body) = send(&app, json_post("/passkeys/register", &registration, Some(&cookie))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["redirect"], "/settings?saved=passkey");
    let (_, _, settings) = send(&app, get("/settings?saved=passkey", Some(&cookie))).await;
    assert!(settings.contains("Laptop") && settings.contains("Synced") && settings.contains("Never used"));
    assert!(settings.contains("Your passkey is ready"));
    // The same browser response cannot be replayed into a second passkey.
    let (status, _, _) = send(&app, json_post("/passkeys/register", &registration, Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Sign in anonymously with the passkey and land on the dashboard.
    let options = passkey_options(&app, "/passkeys/sign-in/options", "", None).await;
    assert_eq!(options["publicKey"]["rpId"], "test.local");
    let assertion = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
    let (status, headers, body) = send(&app, json_post("/passkeys/sign-in", &assertion, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["redirect"], "/dashboard");
    let passkey_session = session_cookie(&headers);
    assert_ne!(passkey_session, cookie);
    let (status, _, dashboard) = send(&app, get("/dashboard", Some(&passkey_session))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(dashboard.contains("owner@example.com"));
    // Replayed assertions fail: the challenge was consumed.
    let (status, headers, _) = send(&app, json_post("/passkeys/sign-in", &assertion, None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!headers.contains_key(header::SET_COOKIE));
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("Last used"), "{settings}");

    // Removing the passkey is a plain settings form; it then no longer signs in.
    let id = created["id"].as_str().unwrap();
    let (status, headers, _) =
        send(&app, form("/settings", &format!("action=remove_passkey&id={id}"), Some(&cookie))).await;
    assert_eq!(
        (status, headers[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/settings?saved=passkey-removed")
    );
    let (_, _, settings) = send(&app, get("/settings?saved=passkey-removed", Some(&cookie))).await;
    assert!(settings.contains("No passkeys yet") && settings.contains("has been removed"));
    let options = passkey_options(&app, "/passkeys/sign-in/options", "", None).await;
    let assertion = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
    let (status, _, body) = send(&app, json_post("/passkeys/sign-in", &assertion, None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("not registered"), "{body}");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn passkey_endpoints_require_the_sites_origin_and_degrade_without_javascript(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let (_, other_cookie) = settings_user(&pool, "other@example.com").await;
    let app = app(pool.clone());
    for path in ["/passkeys/register/options", "/passkeys/register", "/passkeys/sign-in/options", "/passkeys/sign-in"] {
        let mut request = form(path, "password=correct-horse", Some(&cookie));
        request.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert_eq!(send(&app, request).await.0, StatusCode::FORBIDDEN, "{path}");
        let mut request = form(path, "password=correct-horse", Some(&cookie));
        request.headers_mut().remove(header::ORIGIN);
        assert_eq!(send(&app, request).await.0, StatusCode::FORBIDDEN, "{path} without Origin");
    }
    // Malformed JSON is a client error, never a 500.
    let request = Request::post("/passkeys/sign-in")
        .header(header::ORIGIN, "http://test.local")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"id\":"))
        .unwrap();
    assert!(send(&app, request).await.0.is_client_error());
    let (status, _, body) = send(&app, json_post("/passkeys/sign-in", &serde_json::json!({}), None)).await;
    assert!(status.is_client_error(), "{body}");

    // Without JavaScript the add form explains itself instead of posting JSON.
    let (status, _, body) =
        send(&app, form("/settings", "action=passkey&password=correct-horse&name=x", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("needs JavaScript"), "{body}");

    // Only the owner can remove a passkey, and unknown ids are reported.
    let (status, _, body) = send(&app, form("/settings", "action=remove_passkey&id=nope", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("no longer exists"), "{body}");
    let rp = app_auth::RelyingParty::from_site_url("http://test.local", SITE_NAME).unwrap();
    let mut authenticator = app_auth::testing::Authenticator::new(app_auth::testing::Algorithm::Ed25519);
    let options = app_auth::passkeys::start_registration(&pool, &rp, &user, "").await.unwrap();
    let response = serde_json::from_value(authenticator.create_json("http://test.local", &options, "")).unwrap();
    let passkey = app_auth::passkeys::finish_registration(&pool, &rp, user.id, response).await.unwrap();
    let (status, _, _) =
        send(&app, form("/settings", &format!("action=remove_passkey&id={}", passkey.id), Some(&other_cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(app_auth::passkeys::list(&pool, user.id).await.unwrap().len(), 1);
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn admin_access_requires_live_role(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "admin@example.com").await;
    let (_, other_cookie) = settings_user(&pool, "private@example.com").await;
    let app = app(pool.clone());
    let (status, headers, body) = send(&app, get("/admin", None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/sign-in");
    assert!(!body.contains("private@example.com"));
    assert!(!user.is_admin);
    assert_eq!(send(&app, get("/admin", Some("session=invalid"))).await.0, StatusCode::SEE_OTHER);
    let (status, _, body) = send(&app, get("/admin", Some(&cookie))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!body.contains("private@example.com"));
    assert!(!send(&app, get("/dashboard", Some(&cookie))).await.2.contains("href=\"/admin\""));

    app::admin::set_admin(&pool, " ADMIN@example.com ", true).await.unwrap();
    let (status, headers, body) = send(&app, get("/admin", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(body.contains("private@example.com"));
    assert!(body.contains("<td>Admin</td>"));
    assert!(send(&app, get("/dashboard", Some(&cookie))).await.2.contains("href=\"/admin\""));
    assert_eq!(send(&app, get("/admin", Some(&other_cookie))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&app, form("/admin", "is_admin=true", Some(&cookie))).await.0, StatusCode::METHOD_NOT_ALLOWED);

    app::admin::set_admin(&pool, &user.email, false).await.unwrap();
    assert_eq!(send(&app, get("/admin", Some(&cookie))).await.0, StatusCode::FORBIDDEN);
    assert!(!send(&app, get("/dashboard", Some(&cookie))).await.2.contains("href=\"/admin\""));
    assert!(app::admin::set_admin(&pool, "missing@example.com", true).await.is_err());
    assert!(app::admin::set_admin(&pool, "invalid", true).await.is_err());
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn admin_role_cannot_be_set_by_signup_or_profile_forms(pool: PgPool) {
    let app = app(pool.clone());
    let (status, headers, _) = send(
        &app,
        form("/sign-up", "email=signup-admin%40example.com&password=correct-horse&is_admin=true&role=admin", None),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let verified = common::complete(&app, &session_cookie(&headers), "signup-admin@example.com").await;
    let cookie = session_cookie(verified.headers());
    assert_eq!(send(&app, get("/admin", Some(&cookie))).await.0, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        form(
            "/settings",
            "action=profile&name=Admin&email=signup-admin%40example.com&is_admin=true&role=admin",
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(send(&app, get("/admin", Some(&cookie))).await.0, StatusCode::FORBIDDEN);
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn admin_link_is_only_in_the_account_dropdown_and_tracks_live_role(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "menu-admin@example.com").await;
    let app = app(pool.clone());
    let paths = ["/", "/dashboard", "/settings", "/not-found"]
        .into_iter()
        .chain(cfg!(feature = "blog").then_some("/blog"))
        .chain(cfg!(feature = "docs").then_some("/docs"))
        .collect::<Vec<_>>();
    for path in &paths {
        let (_, _, anonymous) = send(&app, get(path, None)).await;
        assert!(!anonymous.contains("href=\"/admin\""));
        let (_, _, ordinary) = send(&app, get(path, Some(&cookie))).await;
        assert!(!ordinary.contains("href=\"/admin\""));
    }
    app::admin::set_admin(&pool, &user.email, true).await.unwrap();
    for path in paths.iter().copied().chain(std::iter::once("/admin")) {
        let (_, _, body) = send(&app, get(path, Some(&cookie))).await;
        let dropdown = body.split("class=\"account-dropdown\"").nth(1).unwrap().split("</details>").next().unwrap();
        assert!(dropdown.contains("href=\"/admin\""), "{path}");
        assert_eq!(body.matches("href=\"/admin\"").count(), 1, "{path}");
        assert!(dropdown.contains("Admin panel"));
        if path == "/admin" {
            assert!(dropdown.contains("href=\"/admin\" aria-current=\"page\""));
        }
        let main = body.split("<main").nth(1).unwrap().split("</main>").next().unwrap();
        assert!(!main.contains("href=\"/admin\""), "{path}");
    }
    app::admin::set_admin(&pool, &user.email, false).await.unwrap();
    for path in paths {
        let (_, _, body) = send(&app, get(path, Some(&cookie))).await;
        assert!(!body.contains("href=\"/admin\""));
    }
    assert_eq!(send(&app, get("/admin", Some(&cookie))).await.0, StatusCode::FORBIDDEN);
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn admin_overview_paginates_and_escapes_account_data(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "admin@example.com").await;
    app::admin::set_admin(&pool, &user.email, true).await.unwrap();
    for n in 0..51 {
        let id = uuid::Uuid::now_v7();
        let email = format!("person{n:02}@example.com");
        sqlx::query!("INSERT INTO users (id, email, password_hash) VALUES ($1, $2, 'unused')", id, email)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query!(
        "INSERT INTO users (id, email, password_hash) VALUES ($1, '<script>@example.com', 'unused')",
        uuid::Uuid::now_v7()
    )
    .execute(&pool)
    .await
    .unwrap();
    let app = app(pool);
    let (status, headers, first) = send(&app, get("/admin", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert!(first.contains("&lt;script&gt;@example.com"));
    assert!(!first.contains("<script>@example.com"));
    let emails = |body: &str| -> Vec<String> {
        body.split("<th scope=\"row\">").skip(1).map(|row| row.split("</th>").next().unwrap().to_owned()).collect()
    };
    let first_emails = emails(&first);
    assert_eq!(first_emails.len(), 50);
    let before = first.split("href=\"/admin?before=").nth(1).unwrap().split('"').next().unwrap();
    let (status, _, second) = send(&app, get(&format!("/admin?before={before}"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let second_emails = emails(&second);
    assert_eq!(second_emails.len(), 3);
    assert!(second_emails.iter().all(|email| !first_emails.contains(email)));
    assert!(!second.contains("Older users"));
    assert!(second.contains("Newest users"));
    assert_eq!(send(&app, get("/admin?before=invalid", Some(&cookie))).await.0, StatusCode::BAD_REQUEST);
    let (_, _, empty) = send(&app, get("/admin?before=00000000-0000-0000-0000-000000000000", Some(&cookie))).await;
    assert!(empty.contains("No users on this page."));
}

// ---------------------------------------------------------------------------
// Optional API
// ---------------------------------------------------------------------------

#[cfg(feature = "auth")]
fn bearer(path: &str, key: Option<&str>) -> Request<Body> {
    let mut b = Request::get(path);
    if let Some(key) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    b.body(Body::empty()).unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn openapi_discovery_alias_follows_the_api_feature(pool: PgPool) {
    let app = app(pool);
    let (status, headers, body) = send(&app, get("/openapi.json", None)).await;
    let (api_status, api_headers, api_body) = send(&app, get("/api/v1/openapi.json", None)).await;
    assert_eq!(status, if cfg!(feature = "api") { StatusCode::OK } else { StatusCode::NOT_FOUND });
    assert_eq!(status, api_status);
    if cfg!(feature = "api") {
        assert_eq!(headers[header::CONTENT_TYPE], "application/json; charset=utf-8");
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
        assert_eq!(headers[header::CONTENT_TYPE], api_headers[header::CONTENT_TYPE]);
        assert_eq!(headers[header::CACHE_CONTROL], api_headers[header::CACHE_CONTROL]);
        assert_eq!(body, api_body);
        let document: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert!(document["paths"]["/api/v1/me"]["get"].is_object());
        assert!(document["paths"]["/openapi.json"].is_null());
    }
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn api_routes_and_settings_panel_follow_the_feature(pool: PgPool) {
    let (_, cookie) = settings_user(&pool, "api@example.com").await;
    let app = app(pool);
    let enabled = cfg!(feature = "api");
    let (status, headers, body) = send(&app, bearer("/api/v1", None)).await;
    assert_eq!(status, if enabled { StatusCode::OK } else { StatusCode::NOT_FOUND });
    assert_eq!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/json"), enabled);
    if enabled {
        let info: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(info["name"], "LVTD");
        assert_eq!(info["revision"], "development");
        assert_eq!(
            info["docs"],
            if cfg!(feature = "docs") { serde_json::json!("/docs/api") } else { serde_json::Value::Null }
        );
        assert_eq!(info["openapi"], "/api/v1/openapi.json");
    }
    // The OpenAPI document and Swagger UI are public and exist only with the API.
    let (status, headers, body) = send(&app, get("/api/v1/openapi.json", None)).await;
    assert_eq!(status, if enabled { StatusCode::OK } else { StatusCode::NOT_FOUND });
    if enabled {
        assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
        let document: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["info"]["title"], "LVTD API");
        assert!(document["paths"]["/api/v1/me"]["get"].is_object(), "{body}");
        let (status, _, page) = send(&app, get("/api/v1/docs", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"data-spec="/api/v1/openapi.json""#), "{page}");
        assert!(page.contains("swagger-ui-dist@") && page.contains(r#"integrity="sha384-"#), "{page}");
        let start = page.find("/assets/api-docs.js?v=").expect("init script");
        let script = &page[start..start + page[start..].find('"').unwrap()];
        assert_eq!(send(&app, get(script, None)).await.0, StatusCode::OK);
    }
    let (status, headers, _) = send(&app, bearer("/api/v1/me", None)).await;
    assert_eq!(status, if enabled { StatusCode::UNAUTHORIZED } else { StatusCode::NOT_FOUND });
    assert_eq!(headers.contains_key(header::WWW_AUTHENTICATE), enabled);
    // A browser session is never an API credential, and the API never redirects.
    let (status, headers, _) = send(&app, get("/api/v1/me", Some(&cookie))).await;
    assert_eq!(status, if enabled { StatusCode::UNAUTHORIZED } else { StatusCode::NOT_FOUND });
    assert!(!headers.contains_key(header::LOCATION));
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert_eq!(settings.contains("value=\"create_api_key\""), enabled);
    assert_eq!(settings.contains("href=\"/docs/api\""), enabled && cfg!(feature = "docs"));
    let (status, _, _) = send(&app, form("/settings", "action=create_api_key&name=x", Some(&cookie))).await;
    assert_eq!(status, if enabled { StatusCode::OK } else { StatusCode::UNPROCESSABLE_ENTITY });
}

#[cfg(feature = "api")]
fn new_key(page: &str) -> String {
    let token = page.split("<pre><code>").nth(1).expect("key shown").split("</code></pre>").next().unwrap();
    assert!(token.starts_with("sr_") && token.len() == 67, "{token}");
    token.to_string()
}

#[cfg(feature = "api")]
#[sqlx::test(migrations = "../migrations")]
async fn api_keys_are_created_shown_once_limited_and_revoked(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "keys@example.com").await;
    let (_, other_cookie) = settings_user(&pool, "other@example.com").await;
    let app = app(pool.clone());
    assert!(send(&app, get("/settings", Some(&cookie))).await.2.contains("You have no API keys."));

    let (status, _, page) = send(&app, form("/settings", "action=create_api_key&name=++Laptop++", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let token = new_key(&page);
    assert_eq!(page.matches(&token).count(), 1, "the full key appears exactly once");
    assert!(page.contains("<th scope=\"row\">Laptop</th>") && page.contains("<td>Never</td>"), "{page}");
    assert!(page.contains(&format!("<code>{}…</code>", &token[..11])));
    let stored = sqlx::query!(
        "SELECT encode(token_hash, 'hex') AS \"hash!\", prefix, name FROM api_keys WHERE user_id = $1",
        user.id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.hash != token && !token.contains(&stored.hash));
    assert_eq!((stored.prefix.as_str(), stored.name.as_str()), (&token[..11], "Laptop"));
    assert!(!send(&app, get("/settings", Some(&cookie))).await.2.contains(&token));

    // The key authenticates the API, records its use, and never acts as a cookie.
    let (status, headers, body) = send(&app, bearer("/api/v1/me", Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let me: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(me["email"], "keys@example.com");
    assert_eq!(me["id"], user.id.to_string());
    assert_eq!(me["is_admin"], false);
    assert!(
        sqlx::query!("SELECT last_used_at FROM api_keys WHERE user_id = $1", user.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .last_used_at
            .is_some()
    );
    assert_eq!(send(&app, get("/dashboard", Some(&format!("session={token}")))).await.0, StatusCode::SEE_OTHER);
    for bad in ["sr_0000", &token.replace("sr_", "xx_")] {
        assert_eq!(send(&app, bearer("/api/v1/me", Some(bad))).await.0, StatusCode::UNAUTHORIZED);
    }
    let (status, _, body) = send(&app, bearer("/api/v1/nope", Some(&token))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "{\"error\":\"Not found.\"}");

    // Names are validated; blank names get a default.
    for name in ["x".repeat(61), "bad%0Aname".into()] {
        let (status, _, page) =
            send(&app, form("/settings", &format!("action=create_api_key&name={name}"), Some(&cookie))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(page.contains("60 characters or fewer") && !page.contains("<pre><code>sr_"), "{page}");
    }
    let (_, _, page) = send(&app, form("/settings", "action=create_api_key&name=", Some(&cookie))).await;
    let unnamed = new_key(&page);
    assert!(page.contains("<th scope=\"row\">API key</th>"));

    // Revoking: cross-site posts, other users and unknown ids are refused.
    let id = sqlx::query!("SELECT id FROM api_keys WHERE prefix = $1", &token[..11]).fetch_one(&pool).await.unwrap().id;
    let mut cross_site = form("/settings", &format!("action=revoke_api_key&id={id}"), Some(&cookie));
    cross_site.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
    assert_eq!(send(&app, cross_site).await.0, StatusCode::FORBIDDEN);
    let (status, _, page) =
        send(&app, form("/settings", &format!("action=revoke_api_key&id={id}"), Some(&other_cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("no longer exists"));
    assert_eq!(send(&app, bearer("/api/v1/me", Some(&token))).await.0, StatusCode::OK);
    let (status, headers, _) =
        send(&app, form("/settings", &format!("action=revoke_api_key&id={id}"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings?revoked=1");
    assert!(send(&app, get("/settings?revoked=1", Some(&cookie))).await.2.contains("has been revoked"));
    let (status, _, body) = send(&app, bearer("/api/v1/me", Some(&token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("revoked"), "{body}");
    assert_eq!(send(&app, bearer("/api/v1/me", Some(&unnamed))).await.0, StatusCode::OK);

    // At most ten keys per account.
    for n in 2..=10 {
        let (status, _, page) =
            send(&app, form("/settings", &format!("action=create_api_key&name=key{n}"), Some(&cookie))).await;
        assert_eq!(status, StatusCode::OK, "{page}");
    }
    let (status, _, page) = send(&app, form("/settings", "action=create_api_key&name=eleven", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(page.contains("already have 10 API keys"), "{page}");
    let (_, _, page) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(page.contains("reached the limit") && !page.contains("value=\"create_api_key\""));

    // Deleting the account deletes its keys.
    let (status, _, _) =
        send(&app, form("/settings", "action=delete&confirmation=DELETE&password=correct-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(send(&app, bearer("/api/v1/me", Some(&unnamed))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(sqlx::query!("SELECT count(*) AS \"count!\" FROM api_keys").fetch_one(&pool).await.unwrap().count, 0);
}

#[cfg(feature = "stripe")]
#[sqlx::test(migrations = "../migrations")]
async fn admins_have_complimentary_access_without_stripe_configuration(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "unconfigured-admin@example.com").await;
    let app = app(pool.clone());
    app::admin::set_admin(&pool, &user.email, true).await.unwrap();
    assert!(app::billing::require_access(&pool, user.id).await.is_ok());
    for request in [get("/billing", Some(&cookie)), form("/billing", "mode=payment", Some(&cookie))] {
        let (status, _, body) = send(&app, request).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Admin access"));
        assert!(!body.contains("Billing is not configured yet"));
    }
    assert!(send(&app, get("/settings", Some(&cookie))).await.2.contains("No payment is required"));
    app::admin::set_admin(&pool, &user.email, false).await.unwrap();
    assert!(app::billing::require_access(&pool, user.id).await.is_err());
    assert_eq!(send(&app, form("/billing", "mode=payment", Some(&cookie))).await.0, StatusCode::SERVICE_UNAVAILABLE);
}

// MCP server
// ---------------------------------------------------------------------------

/// A `POST /mcp` carrying one JSON-RPC message, as MCP clients send it.
#[cfg(feature = "auth")]
fn mcp(key: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::post("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(key) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn mcp_route_and_settings_link_follow_the_feature(pool: PgPool) {
    let (_, cookie) = settings_user(&pool, "mcp@example.com").await;
    let app = app(pool);
    let enabled = cfg!(feature = "mcp");
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
    let (status, headers, _) = send(&app, mcp(None, initialize)).await;
    assert_eq!(status, if enabled { StatusCode::UNAUTHORIZED } else { StatusCode::NOT_FOUND });
    assert_eq!(headers.contains_key(header::WWW_AUTHENTICATE), enabled);
    // A browser session is never an MCP credential.
    let mut with_cookie = mcp(None, initialize);
    with_cookie.headers_mut().insert(header::COOKIE, cookie.parse().unwrap());
    let (status, headers, _) = send(&app, with_cookie).await;
    assert_eq!(status, if enabled { StatusCode::UNAUTHORIZED } else { StatusCode::NOT_FOUND });
    assert!(!headers.contains_key(header::LOCATION));
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert_eq!(settings.contains("href=\"/docs/mcp\""), enabled && cfg!(feature = "docs"));
}

#[cfg(feature = "mcp")]
#[sqlx::test(migrations = "../migrations")]
async fn mcp_serves_the_account_tool_with_api_keys(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "agent@example.com").await;
    let app = app(pool.clone());
    let (_, _, page) = send(&app, form("/settings", "action=create_api_key&name=claude", Some(&cookie))).await;
    let key = new_key(&page);
    let rpc = |method: &str, params: serde_json::Value| {
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()
    };
    let call = |app: &Router, body: String| {
        let app = app.clone();
        let key = key.clone();
        async move {
            let (status, headers, body) = send(&app, mcp(Some(&key), &body)).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(headers[header::CONTENT_TYPE], "application/json; charset=utf-8");
            assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
            assert!(!headers.contains_key("mcp-session-id"), "stateless servers assign no session");
            serde_json::from_str::<serde_json::Value>(&body).unwrap()
        }
    };

    let init =
        call(&app, rpc("initialize", serde_json::json!({ "protocolVersion": "2025-03-26", "capabilities": {} }))).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["serverInfo"]["name"], "lvtd-dev");
    assert_eq!(init["result"]["serverInfo"]["title"], SITE_NAME);
    assert!(init["result"]["instructions"].as_str().unwrap().contains("get_account"));
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], false);
    let (status, _, body) =
        send(&app, mcp(Some(&key), r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)).await;
    assert_eq!((status, body.as_str()), (StatusCode::ACCEPTED, ""));

    let tools = call(&app, rpc("tools/list", serde_json::json!({}))).await;
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["get_account"]);

    let (status, _, me) = send(&app, bearer("/api/v1/me", Some(&key))).await;
    assert_eq!(status, StatusCode::OK);
    let me: serde_json::Value = serde_json::from_str(&me).unwrap();
    let account = call(&app, rpc("tools/call", serde_json::json!({ "name": "get_account", "arguments": {} }))).await;
    let result = &account["result"];
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"], me, "the tool and the API agree");
    assert_eq!(result["structuredContent"]["id"], user.id.to_string());
    assert_eq!(result["content"][0]["type"], "text");
    assert!(result["content"][0]["text"].as_str().unwrap().contains("agent@example.com"));

    let unknown = call(&app, rpc("tools/call", serde_json::json!({ "name": "list_widgets" }))).await;
    assert_eq!(unknown["error"]["code"], -32602);
    assert!(unknown["error"]["message"].as_str().unwrap().contains("get_account"));
    let missing = call(&app, rpc("resources/list", serde_json::json!({}))).await;
    assert_eq!(missing["error"]["code"], -32601);
    let (status, _, _) = send(&app, mcp(Some(&key), "not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(&app, bearer("/mcp", Some(&key))).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "no SSE stream to open");
    // Cross-site browser posts are blocked before any JSON-RPC handling.
    let mut cross_site = mcp(Some(&key), &rpc("ping", serde_json::json!({})));
    cross_site.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
    assert_eq!(send(&app, cross_site).await.0, StatusCode::FORBIDDEN);

    // Revoking the key disconnects the agent on its next call.
    let id = sqlx::query!("SELECT id FROM api_keys WHERE prefix = $1", &key[..11]).fetch_one(&pool).await.unwrap().id;
    assert_eq!(
        send(&app, form("/settings", &format!("action=revoke_api_key&id={id}"), Some(&cookie))).await.0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(send(&app, mcp(Some(&key), &rpc("ping", serde_json::json!({})))).await.0, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// OAuth 2.1 for the API and MCP server
// ---------------------------------------------------------------------------

/// A server-to-server request, as OAuth clients send them: no Origin, no cookies.
#[cfg(feature = "auth")]
fn client_post(path: &str, content_type: &str, body: &str) -> Request<Body> {
    Request::post(path).header(header::CONTENT_TYPE, content_type).body(Body::from(body.to_string())).unwrap()
}

#[cfg(feature = "auth")]
fn json_of(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

#[cfg(feature = "mcp")]
fn query_pairs(url: &str) -> Vec<(String, String)> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or_default();
    form_urlencoded_pairs(query)
}

#[cfg(feature = "mcp")]
fn form_urlencoded_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| {
                let s = s.replace('+', " ");
                let mut out = Vec::new();
                let bytes = s.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'%' && i + 2 < bytes.len() {
                        out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                        i += 3;
                    } else {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
                String::from_utf8(out).unwrap()
            };
            (decode(k), decode(v))
        })
        .collect()
}

#[cfg(feature = "mcp")]
fn param<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn oauth_discovery_follows_the_api_feature(pool: PgPool) {
    let app = app(pool);
    let enabled = cfg!(feature = "api");
    let (status, headers, body) = send(&app, get("/.well-known/oauth-authorization-server", None)).await;
    assert_eq!(status, if enabled { StatusCode::OK } else { StatusCode::NOT_FOUND });
    let (status, _, resource) = send(&app, get("/.well-known/oauth-protected-resource", None)).await;
    assert_eq!(status, if enabled { StatusCode::OK } else { StatusCode::NOT_FOUND });
    if !enabled {
        return;
    }
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json; charset=utf-8");
    let metadata = json_of(&body);
    assert_eq!(metadata["issuer"], "http://test.local");
    assert_eq!(metadata["authorization_endpoint"], "http://test.local/oauth/authorize");
    assert_eq!(metadata["token_endpoint"], "http://test.local/oauth/token");
    assert_eq!(metadata["registration_endpoint"], "http://test.local/oauth/register");
    assert_eq!(metadata["code_challenge_methods_supported"], serde_json::json!(["S256"]));
    assert_eq!(metadata["service_documentation"].is_string(), cfg!(feature = "docs"));
    let resource = json_of(&resource);
    assert_eq!(resource["resource"], "http://test.local");
    assert_eq!(resource["authorization_servers"], serde_json::json!(["http://test.local"]));
    assert_eq!(resource["resource_name"], SITE_NAME);
    // A 401 tells clients where the resource metadata is (RFC 9728 §5.1).
    let (status, headers, _) = send(&app, bearer("/api/v1/me", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap().to_owned();
    assert!(challenge.starts_with("Bearer realm=\"lvtd-dev\""), "{challenge}");
    assert!(
        challenge.contains("resource_metadata=\"http://test.local/.well-known/oauth-protected-resource\""),
        "{challenge}"
    );
    let (status, headers, body) = send(&app, mcp(None, "{}")).await;
    if cfg!(feature = "mcp") {
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap().to_owned();
        assert!(
            challenge.contains("resource_metadata=\"http://test.local/.well-known/oauth-protected-resource/mcp\""),
            "{challenge}"
        );
        let (status, _, body) = send(&app, get("/.well-known/oauth-protected-resource/mcp", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_of(&body)["resource"], "http://test.local/mcp");
    } else {
        assert_eq!(send(&app, get("/.well-known/oauth-protected-resource/mcp", None)).await.0, StatusCode::NOT_FOUND);
    }
    // The endpoints are reachable from other origins (clients call them from their servers).
    let mut cross_site = client_post("/oauth/token", "application/x-www-form-urlencoded", "grant_type=refresh_token");
    cross_site.headers_mut().insert(header::ORIGIN, "https://claude.ai".parse().unwrap());
    let (status, headers, body) = send(&app, cross_site).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(json_of(&body)["error"], "invalid_client");
}

#[cfg(feature = "mcp")]
#[sqlx::test(migrations = "../migrations")]
async fn oauth_connects_an_agent_with_consent_and_rotating_tokens(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "oauth@example.com").await;
    let app = app(pool.clone());

    // 1. The client registers itself (RFC 7591): a public client with PKCE.
    let registration = serde_json::json!({
        "client_name": "Test Agent", "client_uri": "https://agent.example",
        "redirect_uris": ["https://agent.example/callback"],
        "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"],
    });
    let (status, _, body) =
        send(&app, client_post("/oauth/register", "application/json", &registration.to_string())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let client_id = json_of(&body)["client_id"].as_str().unwrap().to_owned();

    // 2. The browser arrives signed out: the request is remembered and sign-in comes first.
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let authorize = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fagent.example%2Fcallback\
         &state=st4te&code_challenge={CHALLENGE}&code_challenge_method=S256&resource=http%3A%2F%2Ftest.local%2Fmcp&scope=mcp"
    );
    let (status, headers, _) = send(&app, get(&authorize, None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/sign-in");
    let remembered = headers[header::SET_COOKIE].to_str().unwrap().to_owned();
    assert!(remembered.starts_with("oauth_continue=") && remembered.contains("HttpOnly"), "{remembered}");
    let continue_cookie = remembered.split(';').next().unwrap().to_owned();
    // Signed in (by any method), the dashboard sends the browser back to the request, once.
    let (status, headers, _) = send(&app, get("/dashboard", Some(&format!("{cookie}; {continue_cookie}")))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers[header::LOCATION].to_str().unwrap().to_owned();
    assert!(location.starts_with("/oauth/authorize?"), "{location}");
    assert_eq!(param(&query_pairs(&location), "client_id"), Some(client_id.as_str()));
    assert!(headers[header::SET_COOKIE].to_str().unwrap().contains("oauth_continue=; "), "cleared");
    assert_eq!(send(&app, get("/dashboard", Some(&cookie))).await.0, StatusCode::OK);

    // 3. The consent page names the client and replays the request as hidden fields.
    let (status, headers, page) = send(&app, get(&location, Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert!(
        page.contains("Connect Test Agent") && page.contains("oauth@example.com") && page.contains("agent.example"),
        "{page}"
    );
    assert!(
        page.contains("name=\"decision\" value=\"allow\"") && page.contains("name=\"decision\" value=\"deny\""),
        "{page}"
    );
    assert!(
        page.contains(&format!("name=\"client_id\" value=\"{client_id}\""))
            && page.contains("name=\"state\" value=\"st4te\""),
        "{page}"
    );
    let hidden: Vec<(String, String)> = page
        .split("<input type=\"hidden\" name=\"")
        .skip(1)
        .map(|rest| {
            let name = rest.split('"').next().unwrap().to_owned();
            let value = rest.split("value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
            (name, value)
        })
        .collect();
    assert_eq!(hidden.len(), 8, "{hidden:?}");

    // An unregistered redirect URI is refused on this page, never followed; a bad PKCE method goes back to the client.
    let (status, headers, page) =
        send(&app, get(&location.replace("agent.example%2Fcallback", "evil.example%2Fcb"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!headers.contains_key(header::LOCATION));
    assert!(page.contains("Authorization refused") && page.contains("redirect_uri"), "{page}");
    let (status, headers, _) = send(&app, get(&location.replace("S256", "plain"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let error = headers[header::LOCATION].to_str().unwrap().to_owned();
    assert!(error.starts_with("https://agent.example/callback?"), "{error}");
    assert_eq!(param(&query_pairs(&error), "error"), Some("invalid_request"));
    assert_eq!(param(&query_pairs(&error), "state"), Some("st4te"));

    // 4. Decisions are same-origin form posts. Deny returns an error; allow returns a code.
    let encode = |pairs: &[(String, String)]| {
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.replace(':', "%3A").replace('/', "%2F")))
            .collect::<Vec<_>>()
            .join("&")
    };
    let decision = |choice: &str| format!("{}&decision={choice}", encode(&hidden));
    let mut cross_site = form("/oauth/authorize", &decision("allow"), Some(&cookie));
    cross_site.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
    assert_eq!(send(&app, cross_site).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&app, form("/oauth/authorize", &decision("allow"), None)).await.0,
        StatusCode::SEE_OTHER,
        "anonymous: sign in"
    );
    assert_eq!(send(&app, form("/oauth/authorize", &decision("allow"), None)).await.1[header::LOCATION], "/sign-in");
    let (status, headers, _) = send(&app, form("/oauth/authorize", &decision("deny"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let denied = query_pairs(headers[header::LOCATION].to_str().unwrap());
    assert_eq!((param(&denied, "error"), param(&denied, "state")), (Some("access_denied"), Some("st4te")));
    let (status, headers, _) = send(&app, form("/oauth/authorize", &decision("allow"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let granted = headers[header::LOCATION].to_str().unwrap().to_owned();
    assert!(granted.starts_with("https://agent.example/callback?"), "{granted}");
    let granted = query_pairs(&granted);
    let code = param(&granted, "code").unwrap().to_owned();
    assert_eq!(param(&granted, "state"), Some("st4te"));
    assert_eq!(param(&granted, "iss"), Some("http://test.local"));

    // 5. The code becomes a token pair; the access token works on the MCP server and the API.
    let exchange = format!(
        "grant_type=authorization_code&client_id={client_id}&code={code}&code_verifier={VERIFIER}\
         &redirect_uri=https%3A%2F%2Fagent.example%2Fcallback&resource=http%3A%2F%2Ftest.local%2Fmcp"
    );
    let (status, headers, body) =
        send(&app, client_post("/oauth/token", "application/x-www-form-urlencoded", &exchange)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let tokens = json_of(&body);
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("sro_") && refresh.starts_with("srr_"), "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "mcp");
    let (status, _, body) =
        send(&app, client_post("/oauth/token", "application/x-www-form-urlencoded", &exchange)).await;
    assert_eq!(
        (status, json_of(&body)["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant")),
        "single use"
    );
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
    let (status, _, body) = send(&app, mcp(Some(&access), initialize)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json_of(&body)["result"]["serverInfo"]["title"], SITE_NAME);
    let (status, _, body) = send(&app, bearer("/api/v1/me", Some(&access))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json_of(&body)["id"], user.id.to_string());
    assert_eq!(
        send(&app, bearer("/api/v1/me", Some(&refresh))).await.0,
        StatusCode::UNAUTHORIZED,
        "refresh tokens never authenticate"
    );

    // 6. Refreshing rotates the pair; the settings page lists the connection and can end it.
    let renew = format!("grant_type=refresh_token&client_id={client_id}&refresh_token={refresh}");
    let (status, _, body) = send(&app, client_post("/oauth/token", "application/x-www-form-urlencoded", &renew)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rotated = json_of(&body)["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        send(&app, bearer("/api/v1/me", Some(&access))).await.0,
        StatusCode::UNAUTHORIZED,
        "the old token is gone"
    );
    assert_eq!(send(&app, bearer("/api/v1/me", Some(&rotated))).await.0, StatusCode::OK);
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("id=\"connected-apps\"") && settings.contains("Test Agent"), "{settings}");
    assert!(settings.contains("value=\"disconnect_app\""), "{settings}");
    let grant_id =
        sqlx::query!("SELECT id FROM oauth_tokens WHERE user_id = $1", user.id).fetch_one(&pool).await.unwrap().id;
    let (_, other) = settings_user(&pool, "other@example.com").await;
    let (status, _, page) =
        send(&app, form("/settings", &format!("action=disconnect_app&id={grant_id}"), Some(&other))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "someone else's grant: {page}");
    let (status, headers, _) =
        send(&app, form("/settings", &format!("action=disconnect_app&id={grant_id}"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings?revoked=app");
    let (_, _, settings) = send(&app, get("/settings?revoked=app", Some(&cookie))).await;
    assert!(
        settings.contains("has been disconnected") && settings.contains("No applications are connected."),
        "{settings}"
    );
    assert_eq!(send(&app, bearer("/api/v1/me", Some(&rotated))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, mcp(Some(&rotated), initialize)).await.0, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Two-factor authentication
// ---------------------------------------------------------------------------

/// The secret the settings page issued, read back so the test can play the authenticator app.
#[cfg(feature = "auth")]
async fn totp_secret(pool: &PgPool, user_id: uuid::Uuid) -> Vec<u8> {
    sqlx::query!("SELECT secret FROM totp_secrets WHERE user_id = $1", user_id).fetch_one(pool).await.unwrap().secret
}

#[cfg(feature = "auth")]
fn totp_now(secret: &[u8], offset: i64) -> String {
    app_auth::totp::code(secret, app_auth::totp::step_at(time::OffsetDateTime::now_utc()) + offset)
}

/// The eight recovery codes rendered by a settings response.
#[cfg(feature = "auth")]
fn recovery_codes(html: &str) -> Vec<String> {
    let list =
        html.split("<ul class=\"recovery-codes\">").nth(1).expect("recovery codes").split("</ul>").next().unwrap();
    list.split("<code>").skip(1).map(|c| c.split("</code>").next().unwrap().to_string()).collect()
}

/// The `two_factor=...` pair from a sign-in response, which must not carry a session.
#[cfg(feature = "auth")]
fn challenge_cookie(headers: &axum::http::HeaderMap) -> String {
    let cookies: Vec<&str> = headers.get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect();
    assert!(!cookies.iter().any(|c| c.starts_with("session=")), "no session before the code: {cookies:?}");
    let raw = cookies.iter().find(|c| c.starts_with("two_factor=")).expect("challenge cookie");
    assert!(raw.contains("HttpOnly") && raw.contains("SameSite=Strict") && raw.contains("Max-Age=600"), "{raw}");
    raw.split(';').next().unwrap().to_string()
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn two_factor_is_enrolled_in_settings_and_required_at_sign_in(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());

    // Off by default; the settings page links to the enrolment page.
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("Turn on two-factor authentication"), "{settings}");
    assert!(settings.contains("href=\"/settings/two-factor\""));
    let (status, headers, _) = send(&app, get("/settings/two-factor", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in"));
    let (status, headers, body) = send(&app, get("/settings/two-factor", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert!(body.contains("action=\"/settings/two-factor\"") && body.contains("value=\"begin\""), "{body}");
    assert!(!body.contains("<svg xmlns"), "no secret before the password is confirmed");

    // Beginning needs the current password; then the page shows the QR code and manual key.
    let (status, _, body) =
        send(&app, form("/settings/two-factor", "action=begin&password=wrong-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("current password"), "{body}");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM totp_secrets").fetch_one(&pool).await.unwrap().n, 0);
    let (status, headers, _) =
        send(&app, form("/settings/two-factor", "action=begin&password=correct-horse", Some(&cookie))).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/settings/two-factor"));
    let secret = totp_secret(&pool, user.id).await;
    let (_, _, body) = send(&app, get("/settings/two-factor", Some(&cookie))).await;
    assert!(body.contains("<svg xmlns=\"http://www.w3.org/2000/svg\"") && body.contains("value=\"confirm\""), "{body}");
    let manual = app_auth::totp::base32_encode(&secret);
    // Project names may contain spaces or punctuation. The auth crate separately
    // tests exact URL encoding; here verify the complete rendered enrolment link.
    let link = app_auth::totp::otpauth_url(SITE_NAME, "owner@example.com", &secret).replace('&', "&amp;");
    assert!(body.contains(&format!("href=\"{link}\"")), "otpauth link: {body}");
    assert!(body.contains(&manual[..4]), "manual key: {body}");
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("Finish setup"), "{settings}");
    // Pending enrolment does not change sign-in yet.
    let (status, headers, _) =
        send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/dashboard"));

    // A wrong code keeps it pending; the right one turns it on and shows the recovery codes once.
    let (status, _, body) = send(&app, form("/settings/two-factor", "action=confirm&code=000000", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("not correct") && body.contains("<svg xmlns"), "{body}");
    let enrol_code = totp_now(&secret, 0);
    let (status, _, body) =
        send(&app, form("/settings/two-factor", &format!("action=confirm&code={enrol_code}"), Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Save these recovery codes now"), "{body}");
    let codes = recovery_codes(&body);
    assert_eq!(codes.len(), 8, "{codes:?}");
    assert!(codes.iter().all(|c| c.len() == 11 && c.as_bytes()[5] == b'-'), "{codes:?}");
    let (_, _, body) = send(&app, get("/settings/two-factor", Some(&cookie))).await;
    assert!(!body.contains("recovery-codes") && body.contains("8 of 8 left"), "shown once: {body}");
    let (_, _, settings) = send(&app, get("/settings", Some(&cookie))).await;
    assert!(settings.contains("Manage two-factor authentication") && settings.contains("8 of 8 recovery codes"));

    // Password sign-in now parks the browser on the code page without a session.
    let (status, headers, _) =
        send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in/two-factor"));
    let challenge = challenge_cookie(&headers);
    let (status, _, _) = send(&app, get("/dashboard", Some(&challenge))).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "the challenge cookie is not a session");
    let (status, headers, _) = send(&app, get("/sign-in/two-factor", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in"));
    let (status, headers, body) = send(&app, get("/sign-in/two-factor", Some(&challenge))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(body.contains("autocomplete=\"one-time-code\"") && body.contains("action=\"/sign-in/two-factor\""));
    let (status, headers, _) = send(&app, form("/sign-in/two-factor", &format!("code={enrol_code}"), None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/sign-in"));

    // The enrolment code is spent; a wrong code says how many attempts remain.
    let (status, headers, body) =
        send(&app, form("/sign-in/two-factor", &format!("code={enrol_code}"), Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("4 attempts left"), "{body}");
    assert!(!headers.contains_key(header::SET_COOKIE));
    let next_code = totp_now(&secret, 1);
    let (status, headers, _) = send(
        &app,
        form("/sign-in/two-factor", &format!("code={}+{}", &next_code[..3], &next_code[3..]), Some(&challenge)),
    )
    .await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/dashboard"));
    let cookies: Vec<String> = headers.get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap().into()).collect();
    assert!(cookies.iter().any(|c| c.starts_with("two_factor=;") && c.contains("Max-Age=0")), "{cookies:?}");
    let session = cookies.iter().find(|c| c.starts_with("session=")).unwrap().split(';').next().unwrap().to_string();
    let (status, _, dashboard) = send(&app, get("/dashboard", Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(dashboard.contains("owner@example.com"));
    // The challenge was consumed with the session.
    let (status, headers, body) =
        send(&app, form("/sign-in/two-factor", &format!("code={next_code}"), Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("sign-in has expired") && body.contains("name=\"email\""), "{body}");
    assert!(headers[header::SET_COOKIE].to_str().unwrap().starts_with("two_factor=;"));

    // A recovery code signs in once, in any case and with or without the dash.
    let (_, headers, _) = send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    let challenge = challenge_cookie(&headers);
    let recovery = codes[2].replace('-', "").to_uppercase();
    let (status, headers, _) =
        send(&app, form("/sign-in/two-factor", &format!("code={recovery}"), Some(&challenge))).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/dashboard"));
    let (_, _, body) = send(&app, get("/settings/two-factor", Some(&cookie))).await;
    assert!(body.contains("7 of 8 left"), "{body}");
    let (_, headers, _) = send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    let challenge = challenge_cookie(&headers);
    let (status, _, body) =
        send(&app, form("/sign-in/two-factor", &format!("code={}", codes[2]), Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Five wrong codes discard the challenge and send the user back to the password form.
    for left in [3, 2, 1] {
        let (status, _, body) = send(&app, form("/sign-in/two-factor", "code=000000", Some(&challenge))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains(&format!("{left} attempt")), "{body}");
    }
    let (status, headers, body) = send(&app, form("/sign-in/two-factor", "code=000000", Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Too many incorrect codes") && body.contains("name=\"password\""), "{body}");
    assert!(headers[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let (status, headers, _) =
        send(&app, form("/sign-in/two-factor", &format!("code={}", codes[3]), Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!headers.get_all(header::SET_COOKIE).iter().any(|c| c.to_str().unwrap().starts_with("session=")));

    // New recovery codes replace the old ones and need the password.
    let (status, _, _) =
        send(&app, form("/settings/two-factor", "action=recovery_codes&password=wrong-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, body) =
        send(&app, form("/settings/two-factor", "action=recovery_codes&password=correct-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    let fresh = recovery_codes(&body);
    assert_eq!(fresh.len(), 8);
    assert!(fresh.iter().all(|c| !codes.contains(c)));
    let (_, headers, _) = send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    let challenge = challenge_cookie(&headers);
    let (status, _, _) = send(&app, form("/sign-in/two-factor", &format!("code={}", codes[4]), Some(&challenge))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = send(&app, form("/sign-in/two-factor", &format!("code={}", fresh[0]), Some(&challenge))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Turning it off needs the password; the password alone then signs in again.
    let (status, _, _) =
        send(&app, form("/settings/two-factor", "action=disable&password=wrong-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(app_auth::two_factor::is_enabled(&pool, user.id).await.unwrap());
    let (status, headers, _) =
        send(&app, form("/settings/two-factor", "action=disable&password=correct-horse", Some(&cookie))).await;
    assert_eq!(
        (status, headers[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/settings/two-factor?saved=off")
    );
    let (_, _, body) = send(&app, get("/settings/two-factor?saved=off", Some(&cookie))).await;
    assert!(body.contains("Two-factor authentication is off") && body.contains("value=\"begin\""), "{body}");
    assert_eq!(sqlx::query!("SELECT count(*) AS \"n!\" FROM recovery_codes").fetch_one(&pool).await.unwrap().n, 0);
    let (status, headers, _) =
        send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/dashboard"));
    assert!(headers[header::SET_COOKIE].to_str().unwrap().starts_with("session="));
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn two_factor_setup_can_be_cancelled_and_passkeys_skip_the_code(pool: PgPool) {
    use app_auth::testing::{Algorithm, Authenticator};
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());

    // Cancelling drops the pending secret; the forms refuse cross-origin posts.
    send(&app, form("/settings/two-factor", "action=begin&password=correct-horse", Some(&cookie))).await;
    let mut request = form("/settings/two-factor", "action=cancel", Some(&cookie));
    request.headers_mut().insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
    assert_eq!(send(&app, request).await.0, StatusCode::FORBIDDEN);
    let (status, headers, _) = send(&app, form("/settings/two-factor", "action=cancel", Some(&cookie))).await;
    assert_eq!(
        (status, headers[header::LOCATION].to_str().unwrap()),
        (StatusCode::SEE_OTHER, "/settings/two-factor?saved=cancelled")
    );
    let (_, _, body) = send(&app, get("/settings/two-factor?saved=cancelled", Some(&cookie))).await;
    assert!(body.contains("Setup cancelled") && !body.contains("<svg xmlns"), "{body}");
    let (status, _, body) = send(&app, form("/settings/two-factor", "action=confirm&code=123456", Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("not being set up"), "{body}");

    // Enrol, then add a passkey: passkey sign-in is already two factors and gets a session directly.
    send(&app, form("/settings/two-factor", "action=begin&password=correct-horse", Some(&cookie))).await;
    let secret = totp_secret(&pool, user.id).await;
    let (status, _, _) = send(
        &app,
        form("/settings/two-factor", &format!("action=confirm&code={}", totp_now(&secret, 0)), Some(&cookie)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let options = passkey_options(&app, "/passkeys/register/options", "password=correct-horse", Some(&cookie)).await;
    let mut authenticator = Authenticator::new(Algorithm::Es256);
    let registration = authenticator.create_json("http://test.local", &options, "Laptop");
    assert_eq!(send(&app, json_post("/passkeys/register", &registration, Some(&cookie))).await.0, StatusCode::CREATED);
    let options = passkey_options(&app, "/passkeys/sign-in/options", "", None).await;
    let assertion = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
    let (status, headers, body) = send(&app, json_post("/passkeys/sign-in", &assertion, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers[header::SET_COOKIE].to_str().unwrap().starts_with("session="));
    // A signed-in browser is sent away from the code page.
    let session = session_cookie(&headers);
    let (status, headers, _) = send(&app, get("/sign-in/two-factor", Some(&session))).await;
    assert_eq!((status, headers[header::LOCATION].to_str().unwrap()), (StatusCode::SEE_OTHER, "/dashboard"));

    // Deleting the account removes the secret, the codes and any challenge in flight.
    send(&app, form("/sign-in", "email=owner%40example.com&password=correct-horse", None)).await;
    let (status, _, _) =
        send(&app, form("/settings", "action=delete&confirmation=DELETE&password=correct-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let counts = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM totp_secrets) AS "secrets!",
                  (SELECT count(*) FROM recovery_codes) AS "codes!",
                  (SELECT count(*) FROM two_factor_challenges) AS "challenges!""#
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((counts.secrets, counts.codes, counts.challenges), (0, 0, 0));
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn two_factor_rejects_stale_challenges_and_oversized_passwords_without_side_effects(pool: PgPool) {
    let (user, cookie) = settings_user(&pool, "stale-factor@example.com").await;
    let app = app(pool.clone());
    let oversized = "x".repeat(app_auth::password::MAX_LEN + 1);
    for action in ["begin", "recovery_codes", "disable"] {
        let (status, headers, body) =
            send(&app, form("/settings/two-factor", &format!("action={action}&password={oversized}"), Some(&cookie)))
                .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{action}");
        assert!(body.contains("current password"));
        assert!(!headers.contains_key(header::SET_COOKIE));
        assert_eq!(app_auth::two_factor::status(&pool, user.id).await.unwrap(), app_auth::two_factor::Status::Off);
    }
    let (status, headers, _) =
        send(&app, form("/settings/two-factor", "action=disable&password=correct-horse", Some(&cookie))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/settings/two-factor?saved=off");
    let token = app_auth::two_factor::start_challenge(&pool, user.id).await.unwrap();
    app_auth::two_factor::disable(&pool, user.id).await.unwrap();
    let stale = format!("two_factor={token}");
    let (status, headers, _) = send(&app, get("/sign-in/two-factor", Some(&stale))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/sign-in");
    let (status, headers, body) = send(&app, form("/sign-in/two-factor", "code=123456", Some(&stale))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("expired"));
    let cookies: Vec<_> = headers.get_all(header::SET_COOKIE).iter().map(|h| h.to_str().unwrap()).collect();
    assert_eq!(cookies.len(), 1);
    assert!(cookies[0].starts_with("two_factor=;") && cookies[0].contains("Max-Age=0"));
    assert!(
        app_auth::sessions::lookup(&pool, cookie.strip_prefix("session=").unwrap()).await.unwrap().is_some(),
        "unrelated session remains valid"
    );
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count,
        Some(1),
        "no session was created by the stale challenge"
    );
}

/// Parse the actual raw-text payload, rather than accepting HTML-escaped JSON.
fn json_ld(body: &str) -> Vec<serde_json::Value> {
    body.split("<script type=\"application/ld+json\">")
        .skip(1)
        .map(|part| serde_json::from_str(part.split("</script>").next().unwrap()).unwrap())
        .collect()
}

#[sqlx::test(migrations = "../migrations")]
async fn public_pages_have_linked_json_ld_and_private_pages_do_not(pool: PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local/".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, router) = app::build(cfg, pool);
    let mut paths = app::indexnow_paths(&state);

    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), state.structured_data.len());
    for path in paths {
        let (status, _, body) = send(&router, get(&path, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let data = json_ld(&body);
        let graph = data[0]["@graph"].as_array().unwrap();
        assert_eq!(data[0]["@context"], "https://schema.org");
        assert_eq!(graph.len(), 3);
        let product = &graph[0];
        assert_eq!(product["@type"], "SoftwareApplication");
        assert_eq!(product["name"], app::views::SITE_NAME);
        assert_eq!(product["description"], app::views::SITE_DESCRIPTION);
        assert_eq!(product["url"], "http://test.local/");
        assert_eq!(product["@id"], "http://test.local/#product");
        assert!(product.get("offers").is_none());
        assert!(product.get("sameAs").is_none());
        assert!(product.get("publisher").is_none());
        assert_eq!(graph[1]["@type"], "WebSite");
        assert_eq!(graph[1]["@id"], "http://test.local/#website");
        assert_eq!(graph[1]["url"], "http://test.local/");
        assert_eq!(graph[2]["url"], format!("http://test.local{path}"));
        assert_eq!(graph[2]["@id"], format!("http://test.local{path}#page"));
        assert_eq!(graph[2]["about"]["@id"], product["@id"]);
        assert_eq!(graph[2]["isPartOf"]["@id"], graph[1]["@id"]);
        assert!(!graph[2]["name"].as_str().unwrap().is_empty());
        assert!(!graph[2]["description"].as_str().unwrap().is_empty());
        assert_eq!(graph[2]["inLanguage"], "en");
        if path == "/" {
            assert_eq!(graph[2]["@type"], "WebPage");
            assert_eq!(graph[2]["mainEntity"]["@id"], product["@id"]);
        }
        if path == "/blog" {
            assert_eq!(graph[2]["@type"], "Blog");
        }
        if path.starts_with("/docs") {
            assert_eq!(graph[2]["@type"], "TechArticle");
        }
    }

    #[cfg(feature = "docs")]
    for doc in std::iter::once(&state.docs.welcome).chain(&state.docs.pages) {
        let (_, _, body) = send(&router, get(&doc.path, None)).await;
        let data = json_ld(&body);
        assert_eq!(data[0]["@graph"][2]["headline"], doc.title);
        assert_eq!(data[0]["@graph"][2]["description"], doc.description);
    }

    #[cfg(feature = "blog")]
    for post in &state.blog.posts {
        let (_, _, body) = send(&router, get(&post.path, None)).await;
        let data = json_ld(&body);
        let article = &data[0]["@graph"][2];
        assert_eq!(article["@type"], "BlogPosting");
        assert_eq!(article["name"], post.title);
        assert_eq!(article["description"], post.description);
        assert_eq!(article["author"]["name"], post.author);
        assert_eq!(article["datePublished"], post.date.to_string());
    }
    for path in [
        "/sign-in",
        "/sign-up",
        "/dashboard",
        "/settings",
        "/admin",
        "/missing",
        "/blog/missing",
        "/docs/missing",
        "/api/v1/docs",
    ] {
        let (_, _, body) = send(&router, get(path, None)).await;
        assert!(json_ld(&body).is_empty(), "{path}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn marketing_pages_negotiate_markdown(pool: PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, router) = app::build(cfg, pool);
    let paths = app::indexnow_paths(&state);
    for path in paths {
        for (accept, mime) in [
            ("text/markdown", "text/markdown"),
            ("text/html", "text/html"),
            ("*/*", "text/html"),
            ("text/markdown;q=0", "text/html"),
        ] {
            let (status, headers, body) =
                send(&router, Request::get(&path).header(header::ACCEPT, accept).body(Body::empty()).unwrap()).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert_eq!(headers[header::CONTENT_TYPE], format!("{mime}; charset=utf-8"), "{path}");
            assert!(
                headers.get_all(header::VARY).iter().any(|value| value
                    .to_str()
                    .unwrap()
                    .split(',')
                    .any(|v| v.trim().eq_ignore_ascii_case("Accept"))),
                "{path}"
            );
            assert!(!body.trim().is_empty(), "{path}");
            assert_eq!(body.contains("<!DOCTYPE html>"), mime == "text/html", "{path}");
            if mime == "text/markdown" {
                assert!(!body.contains("<script"), "{path}");
                assert!(!body.contains("<svg"), "{path}");
            }
        }
        let (status, headers, body) =
            send(&router, Request::head(&path).header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap())
                .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], "text/markdown; charset=utf-8");
        assert_eq!(headers[header::VARY], "Accept");
        assert!(body.is_empty(), "{path}");
        let (status, _, _) = send(&router, form(&path, "", None)).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }
    let (_, _, body) =
        send(&router, Request::get("/").header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap()).await;
    assert!(body.contains("Say what it does."));
    for path in ["/healthz", "/sitemap.xml"] {
        let (status, headers, _) =
            send(&router, Request::get(path).header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap())
                .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/markdown"));
    }
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn markdown_is_public_while_browser_navigation_stays_personalized(pool: PgPool) {
    let user =
        app_auth::users::sign_up(&pool, "markdown-private@example.com", "correct-horse-battery".into()).await.unwrap();
    let token = app_auth::sessions::create(&pool, user.id, time::Duration::days(1)).await.unwrap();
    let cookie = format!("session={token}");
    let router = app(pool);
    let (_, _, html) = send(&router, get("/", Some(&cookie))).await;
    assert!(html.contains("markdown-private@example.com"));
    let (_, _, anonymous) =
        send(&router, Request::get("/").header(header::ACCEPT, "text/markdown").body(Body::empty()).unwrap()).await;
    let (_, _, signed_in) = send(
        &router,
        Request::get("/")
            .header(header::ACCEPT, "text/markdown")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(signed_in, anonymous);
    assert!(!signed_in.contains("markdown-private@example.com"));
    for path in ["/sign-in", "/settings"] {
        let (status, headers, body) = send(
            &router,
            Request::get(path)
                .header(header::ACCEPT, "text/markdown")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        // Sign-in can redirect an existing session; settings must remain an HTML form.
        if status == StatusCode::OK {
            assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
            assert!(body.contains("<form"));
        } else {
            assert!(status.is_redirection());
        }
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn every_sitemap_page_has_complete_unique_share_metadata(pool: PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local/".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, router) = app::build(cfg, pool);
    let (_, _, sitemap) = send(&router, get("/sitemap.xml", None)).await;
    let mut paths: Vec<_> = sitemap
        .split("<loc>")
        .skip(1)
        .map(|entry| entry.split("</loc>").next().unwrap().strip_prefix("http://test.local").unwrap().to_owned())
        .collect();
    paths.sort();
    assert_eq!(app::indexnow_paths(&state), paths);
    let (status, headers, robots) = send(&router, get("/robots.txt", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(robots, "User-agent: *\nAllow: /\nSitemap: http://test.local/sitemap.xml\n");
    let mut titles = std::collections::HashSet::new();
    let mut descriptions = std::collections::HashSet::new();
    let mut images = std::collections::HashSet::new();
    for entry in sitemap.split("<loc>").skip(1) {
        let url = entry.split("</loc>").next().unwrap();
        let path = url.strip_prefix("http://test.local").unwrap();
        let request = Request::get(format!("{path}?utm_source=share"))
            .header(header::HOST, "untrusted.example")
            .body(Body::empty())
            .unwrap();
        let (status, headers, body) = send(&router, request).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!headers.contains_key("x-robots-tag"), "{path}");
        let head = body.split("</head>").next().unwrap();
        let value = |prefix: &str| {
            assert_eq!(head.matches(prefix).count(), 1, "{path}: {prefix}");
            head.split(prefix).nth(1).unwrap().split('"').next().unwrap().to_owned()
        };
        assert_eq!(value("rel=\"canonical\" href=\""), url);
        assert_eq!(value("property=\"og:url\" content=\""), url);
        assert_eq!(value("name=\"robots\" content=\""), "index, follow, max-image-preview:large");
        assert_eq!(value("name=\"twitter:card\" content=\""), "summary_large_image");
        let title = head.split("<title>").nth(1).unwrap().split("</title>").next().unwrap();
        assert!(!title.is_empty() && titles.insert(title.to_owned()), "{path}: duplicate or empty title");
        assert_eq!(value("property=\"og:title\" content=\""), title);
        assert_eq!(value("name=\"twitter:title\" content=\""), title);
        let description = value("name=\"description\" content=\"");
        assert!(
            !description.is_empty() && descriptions.insert(description.clone()),
            "{path}: duplicate or empty description"
        );
        assert_eq!(value("property=\"og:description\" content=\""), description);
        assert_eq!(value("name=\"twitter:description\" content=\""), description);
        assert!(!value("property=\"og:site_name\" content=\"").is_empty());
        let kind = value("property=\"og:type\" content=\"");
        assert_eq!(kind, if path.starts_with("/blog/") || path.starts_with("/vs/") { "article" } else { "website" });
        let image = value("property=\"og:image\" content=\"");
        assert!(images.insert(image.clone()), "{path}: duplicate image");
        assert_eq!(value("name=\"twitter:image\" content=\""), image);
        let alt = value("property=\"og:image:alt\" content=\"");
        assert!(!alt.is_empty(), "{path}");
        assert_eq!(value("name=\"twitter:image:alt\" content=\""), alt);
        assert_eq!(value("property=\"og:image:type\" content=\""), "image/png");
        assert_eq!(value("property=\"og:image:width\" content=\""), "1200");
        assert_eq!(value("property=\"og:image:height\" content=\""), "630");
        let image_path = image.strip_prefix("http://test.local").unwrap();
        let response = router.clone().oneshot(get(image_path, None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{image}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let png = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 1200);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 630);
    }
    for path in ["/missing", "/sign-in", "/sign-up"] {
        let (_, _, body) = send(&router, get(path, None)).await;
        assert!(body.contains("name=\"robots\" content=\"noindex, follow\""), "{path}");
    }
}

#[sqlx::test(migrations = "../migrations")]
async fn robots_advertises_the_configured_sitemap(pool: PgPool) {
    let router = app(pool);
    let request = Request::get("/robots.txt").header(header::HOST, "untrusted.example").body(Body::empty()).unwrap();
    let (status, headers, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    assert!(!headers.contains_key(header::SET_COOKIE));
    assert_eq!(body, "User-agent: *\nAllow: /\nSitemap: http://test.local/sitemap.xml\n");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn rejected_passkey_registrations_consume_challenges_without_adding_credentials(pool: PgPool) {
    use app_auth::testing::{Algorithm, Authenticator};
    use app_auth::webauthn::b64url_encode;
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());
    let oversized = format!("password={}", "x".repeat(app_auth::password::MAX_LEN + 1));
    let (status, headers, body) = send(&app, form("/passkeys/register/options", &oversized, Some(&cookie))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("Enter your current password"), "{body}");
    assert!(!headers.contains_key(header::SET_COOKIE));
    for (fault, expected) in [
        ("origin", "The request came from a different origin."),
        ("rp", "The passkey belongs to a different site."),
        ("verification", "User verification is required. Use a passkey protected by a PIN or biometrics."),
        ("id", "The credential id does not match."),
    ] {
        let options =
            passkey_options(&app, "/passkeys/register/options", "password=correct-horse", Some(&cookie)).await;
        let mut authenticator = Authenticator::new(Algorithm::Es256);
        let valid = authenticator.create_json("http://test.local", &options, "Laptop");
        authenticator.rp_id = Some(if fault == "rp" { "other.local" } else { "test.local" }.into());
        authenticator.user_verified = fault != "verification";
        let mut rejected = authenticator.create_json(
            if fault == "origin" { "https://evil.example" } else { "http://test.local" },
            &options,
            "Laptop",
        );
        if fault == "id" {
            rejected["rawId"] = serde_json::json!(b64url_encode(b"another credential"));
        }
        let (status, headers, body) = send(&app, json_post("/passkeys/register", &rejected, Some(&cookie))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{fault}: {body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["error"], expected, "{fault}");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert!(!headers.contains_key(header::SET_COOKIE));
        assert!(app_auth::passkeys::list(&pool, user.id).await.unwrap().is_empty());
        // Repairing the response cannot reuse a challenge after verification failed.
        let (status, _, body) = send(&app, json_post("/passkeys/register", &valid, Some(&cookie))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("expired"), "{body}");
    }
    let options = passkey_options(&app, "/passkeys/register/options", "password=correct-horse", Some(&cookie)).await;
    let valid = Authenticator::new(Algorithm::Es256).create_json("http://test.local", &options, "Recovered");
    assert_eq!(send(&app, json_post("/passkeys/register", &valid, Some(&cookie))).await.0, StatusCode::CREATED);
    assert_eq!(app_auth::passkeys::list(&pool, user.id).await.unwrap()[0].name, "Recovered");
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn rejected_passkey_assertions_leave_sessions_and_credential_state_unchanged(pool: PgPool) {
    use app_auth::testing::{Algorithm, Authenticator};
    use app_auth::webauthn::{b64url_decode, b64url_encode};
    let (user, cookie) = settings_user(&pool, "owner@example.com").await;
    let app = app(pool.clone());
    let mut authenticator = Authenticator::new(Algorithm::Ed25519);
    authenticator.sign_count = 10;
    let options = passkey_options(&app, "/passkeys/register/options", "password=correct-horse", Some(&cookie)).await;
    let registration = authenticator.create_json("http://test.local", &options, "Laptop");
    assert_eq!(send(&app, json_post("/passkeys/register", &registration, Some(&cookie))).await.0, StatusCode::CREATED);
    for (fault, expected) in [
        ("rp", "The passkey belongs to a different site."),
        ("verification", "User verification is required. Use a passkey protected by a PIN or biometrics."),
        ("signature", "The signature is invalid."),
        ("handle", "This passkey does not belong to the account it claims."),
        ("counter", "This passkey may have been cloned. Remove it and register a new one."),
        ("short", "Authenticator data is too short."),
        ("trailing", "Trailing bytes in authenticator data."),
        ("presence", "User presence was not confirmed."),
    ] {
        let options = passkey_options(&app, "/passkeys/sign-in/options", "", None).await;
        authenticator.rp_id = Some(if fault == "rp" { "other.local" } else { "test.local" }.into());
        authenticator.user_verified = fault != "verification";
        authenticator.backed_up = false;
        authenticator.sign_count = if fault == "counter" { 9 } else { 10 };
        let mut assertion = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
        match fault {
            "signature" => assertion["response"]["signature"] = serde_json::json!(b64url_encode(&[0; 64])),
            "handle" => {
                assertion["response"]["userHandle"] = serde_json::json!(b64url_encode(uuid::Uuid::now_v7().as_bytes()))
            }
            "short" | "trailing" | "presence" => {
                let mut bytes = b64url_decode(assertion["response"]["authenticatorData"].as_str().unwrap()).unwrap();
                match fault {
                    "short" => bytes.truncate(36),
                    "trailing" => bytes.push(0),
                    _ => bytes[32] &= !1,
                }
                assertion["response"]["authenticatorData"] = serde_json::json!(b64url_encode(&bytes));
            }
            _ => {}
        }
        let (status, headers, body) = send(&app, json_post("/passkeys/sign-in", &assertion, None)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{fault}: {body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["error"], expected, "{fault}");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert!(!headers.contains_key(header::SET_COOKIE));
        let saved =
            sqlx::query!("SELECT sign_count, backed_up, last_used_at FROM passkeys WHERE user_id = $1", user.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(saved.sign_count, 10, "{fault}");
        assert!(saved.backed_up && saved.last_used_at.is_none(), "{fault}");
        assert_eq!(
            sqlx::query_scalar!("SELECT count(*) FROM sessions WHERE user_id = $1", user.id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            Some(1)
        );
        // A rejected assertion burns its challenge, even when the same device repairs it.
        authenticator.rp_id = Some("test.local".into());
        authenticator.user_verified = true;
        authenticator.sign_count = 10;
        let repaired = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
        let (status, headers, body) = send(&app, json_post("/passkeys/sign-in", &repaired, None)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("expired"), "{body}");
        assert!(!headers.contains_key(header::SET_COOKIE));
    }
    let options = passkey_options(&app, "/passkeys/sign-in/options", "", None).await;
    let valid = authenticator.get_json("http://test.local", &options, user.id.as_bytes());
    let (status, headers, _) = send(&app, json_post("/passkeys/sign-in", &valid, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(session_cookie(&headers), cookie);
    let saved = sqlx::query!("SELECT sign_count, backed_up, last_used_at FROM passkeys WHERE user_id = $1", user.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved.sign_count, 12);
    assert!(!saved.backed_up && saved.last_used_at.is_some());
    assert_eq!(
        sqlx::query_scalar!("SELECT count(*) FROM sessions WHERE user_id = $1", user.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        Some(2)
    );
}
