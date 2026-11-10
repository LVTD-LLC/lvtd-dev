//! HTTP routing and handlers.

use crate::AppError;
use crate::views::{self, Page};
use crate::{SharedState, assets, errors};
#[cfg(feature = "auth")]
use app_auth::{CurrentUser, sessions, users};
use app_auth::{MaybeUser, User};
use app_core::http::{same_origin_guard, security_headers};
use axum::Router;
use axum::extract::State;
#[cfg(feature = "auth")]
use axum::extract::{Form, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::Redirect;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
#[cfg(any(feature = "auth", feature = "mcp"))]
use axum::routing::post;
use maud::Markup;
#[cfg(feature = "auth")]
use serde::Deserialize;
use std::time::Duration;
#[cfg(feature = "auth")]
use time::Duration as TimeDuration;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

pub fn router(state: SharedState) -> Router {
    #[allow(unused_mut)]
    let mut public = Router::new();
    #[cfg(feature = "blog")]
    for (index, post) in state.blog.posts.iter().enumerate() {
        public = public.route(
            &post.path,
            public_page(&state, move |state, user| {
                let post = &state.blog.posts[index];
                let page = Page { title: &post.title, path: &post.path, user, body: views::blog::article(post) };
                views::layout_with_metadata(state, page, Some(&post.description), Some((&post.author, post.date)))
            }),
        );
    }
    #[cfg(feature = "docs")]
    for (index, doc) in state.docs.pages.iter().enumerate() {
        public = public.route(
            &doc.path,
            public_page(&state, move |state, user| {
                let doc = &state.docs.pages[index];
                let page = Page { title: &doc.title, path: &doc.path, user, body: views::docs::page(&state.docs, doc) };
                views::layout_with_metadata(state, page, Some(&doc.description), None)
            }),
        );
    }
    for page in crate::studio::PAGES {
        public = public.route(
            page.path,
            public_page(&state, move |state, user| {
                views::layout_with_metadata(
                    state,
                    Page { title: page.title, path: page.path, user, body: crate::studio::body(page) },
                    Some(page.description),
                    None,
                )
            }),
        );
    }
    let router = public
        .route("/", public_page(&state, home))
        .route("/sitemap.xml", get(sitemap))
        .route("/robots.txt", get(robots))
        .route("/about", public_page(&state, about))
        .route("/contact", public_page(&state, contact))
        .route("/tos/", get(|| async { Redirect::permanent("/terms") }))
        .route("/tos", get(|| async { Redirect::permanent("/terms") }))
        .route("/projects", get(|| async { Redirect::permanent("/projects/") }))
        .route("/research", get(|| async { Redirect::permanent("/research/") }))
        .route("/privacy/", get(|| async { Redirect::permanent("/privacy") }))
        .route("/ai-steering/", get(|| async { Redirect::permanent("/ai-steering") }))
        .route("/jev-benchmark/", get(|| async { Redirect::permanent("/jev-benchmark") }))
        .route("/jev-benchamrk", get(|| async { Redirect::permanent("/jev-benchmark") }))
        .route("/jev-benchamrk/", get(|| async { Redirect::permanent("/jev-benchmark") }))
        .route("/jev-benchmark/data.json", get(benchmark_data))
        .route("/jev-benchmark/methodology.md", get(benchmark_methodology))
        .route("/healthz", get(healthz))
        .route("/assets/{*path}", get(assets::serve))
        .fallback(not_found);
    #[cfg(feature = "auth")]
    let router = router
        .route("/sign-up", get(sign_up_page).post(sign_up))
        .route("/verify-email", get(verify_email_page).post(verify_email))
        .route("/verify-email/resend", post(resend_verification))
        .route("/sign-in", get(sign_in_page).post(sign_in))
        .route(
            crate::two_factor::SIGN_IN_PATH,
            get(crate::two_factor::challenge_get).post(crate::two_factor::challenge_post),
        )
        .route("/sign-out", post(sign_out))
        .route("/passkeys/register/options", post(crate::passkeys::register_options))
        .route("/passkeys/register", post(crate::passkeys::register))
        .route("/passkeys/sign-in/options", post(crate::passkeys::sign_in_options))
        .route("/passkeys/sign-in", post(crate::passkeys::sign_in))
        .route("/dashboard", get(dashboard))
        .route("/admin", get(crate::admin::page))
        .route("/settings", get(crate::settings::page).post(crate::settings::submit))
        .route(
            crate::two_factor::SETTINGS_PATH,
            get(crate::two_factor::settings_page).post(crate::two_factor::settings_submit),
        );
    #[cfg(feature = "social-login")]
    let router = crate::social_login::routes(router);
    let router = router.route("/og/{*file}", get(crate::og::serve));
    #[cfg(feature = "stripe")]
    let router = router
        .route("/billing", get(billing_page).post(start_checkout))
        .route("/billing/success", get(billing_success))
        .route("/billing/portal", post(billing_portal));
    #[cfg(feature = "api")]
    let router = router
        .route(crate::api::OPENAPI_PATH, get(crate::api::openapi_json))
        .nest(crate::api::BASE_PATH, crate::api::router())
        // The consent page is a browser form, so it stays behind the same-origin guard.
        .route(
            app_auth::oauth_server::AUTHORIZE_PATH,
            get(crate::oauth_server::authorize_page).post(crate::oauth_server::authorize_decision),
        );
    // Stateless MCP: POST only; axum answers other methods with 405, as the protocol allows.
    #[cfg(feature = "mcp")]
    let router = router.route(crate::mcp::PATH, post(crate::mcp::handle));
    // Browser POSTs require same-origin; only signed webhooks and the OAuth
    // endpoints clients call from their own servers are exempt.
    let router = router.layer(middleware::from_fn(same_origin_guard(state.cfg.site_url.clone())));
    #[cfg(feature = "api")]
    let router = router
        .route(app_auth::oauth_server::SERVER_METADATA_PATH, get(crate::oauth_server::server_metadata))
        .route(app_auth::oauth_server::RESOURCE_METADATA_PATH, get(crate::oauth_server::resource_metadata))
        .route(app_auth::oauth_server::REGISTER_PATH, post(crate::oauth_server::register))
        .route(app_auth::oauth_server::TOKEN_PATH, post(crate::oauth_server::token))
        .route(app_auth::oauth_server::REVOKE_PATH, post(crate::oauth_server::revoke));
    #[cfg(feature = "mcp")]
    let router = router.route(
        &state.oauth_server.resource_metadata_path(crate::mcp::PATH),
        get(crate::oauth_server::mcp_resource_metadata),
    );
    #[cfg(feature = "stripe")]
    let router = router.route("/stripe/webhook", post(stripe_webhook));
    #[cfg(feature = "blog")]
    let router = router
        .route("/blog/", get(|| async { Redirect::permanent("/blog") }))
        .route(
            "/blog/{slug}/",
            get(|axum::extract::Path(slug): axum::extract::Path<String>| async move {
                Redirect::permanent(&format!("/blog/{slug}"))
            }),
        )
        .route("/blog", public_page(&state, blog_index))
        .route("/blog/rss.xml", get(blog_feed))
        .route("/blog/{slug}", get(blog_article));
    #[cfg(feature = "docs")]
    let router = router
        .route("/docs", public_page(&state, docs_index))
        .route("/docs/{slug}", get(docs_article))
        .route("/llms.txt", get(llms_txt))
        .route("/llms-full.txt", get(llms_full_txt));
    // Forms are small. The one multipart upload gets its own, larger limit.
    let router = router.layer(RequestBodyLimitLayer::new(64 * 1024));
    #[cfg(feature = "storage")]
    let router = router
        .merge(
            Router::new()
                .route("/settings/avatar", post(crate::avatars::upload))
                .layer(axum::extract::DefaultBodyLimit::max(crate::avatars::BODY_LIMIT))
                .layer(middleware::from_fn(same_origin_guard(state.cfg.site_url.clone()))),
        )
        .route("/avatars/{*path}", get(crate::avatars::serve));
    #[cfg(any(feature = "posthog-logs", feature = "posthog-traces", feature = "sentry-server"))]
    let router = router.layer(middleware::from_fn_with_state(state.clone(), crate::observability::request));
    #[cfg(feature = "sentry-server")]
    let router = router.layer(middleware::from_fn_with_state(state.clone(), crate::sentry::request));
    #[cfg(feature = "auth")]
    let router = router.layer(middleware::from_fn_with_state(state.clone(), auth_origin));
    let router = router.layer(middleware::from_fn(security_headers));
    // Outside the security headers so provider callbacks never leak their code as a Referer.
    #[cfg(feature = "social-login")]
    let router = router.layer(middleware::from_fn(crate::social_login::private_response));
    router
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(20)))
        .with_state(state)
}

pub(crate) fn html(status: StatusCode, markup: Markup) -> Response {
    (status, [(header::CACHE_CONTROL, "private, no-store")], Html(markup.into_string())).into_response()
}

/// Register public content through this helper so every page supports both formats.
/// Render anonymous HTML once for Markdown; browser requests retain their account menu.
fn public_page(
    state: &SharedState,
    render: impl Fn(&SharedState, Option<&User>) -> Markup + Clone + Send + Sync + 'static,
) -> axum::routing::MethodRouter<SharedState> {
    let prepared = app_core::public_page::PublicPage::new(&render(state, None).into_string());
    get(move |State(state): State<SharedState>, MaybeUser(user): MaybeUser, headers: HeaderMap| {
        let prepared = prepared.clone();
        let render = render.clone();
        async move { prepared.respond(&headers, || html(StatusCode::OK, render(&state, user.as_ref()))) }
    })
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn home(state: &SharedState, user: Option<&User>) -> Markup {
    let page = Page { title: views::HOME_TITLE, path: "/", user, body: views::pages::home() };
    views::layout(state, page)
}

fn about(state: &SharedState, user: Option<&User>) -> Markup {
    let page = Page { title: "About", path: "/about", user, body: views::company::about() };
    views::layout(state, page)
}

fn contact(state: &SharedState, user: Option<&User>) -> Markup {
    let page = Page { title: "Contact", path: "/contact", user, body: views::company::contact() };
    views::layout(state, page)
}

#[cfg(feature = "auth")]
async fn dashboard(State(state): State<SharedState>, CurrentUser(user): CurrentUser, headers: HeaderMap) -> Response {
    #[cfg(feature = "posthog")]
    crate::analytics::record_user(&user);
    // Every sign-in path lands here; an interrupted OAuth consent resumes first.
    #[cfg(feature = "api")]
    if let Some(resume) = crate::oauth_server::resume(&headers, state.cfg.secure_cookies()) {
        return resume;
    }
    #[cfg(not(feature = "api"))]
    let _ = headers;
    let page = Page { title: "Dashboard", path: "/dashboard", user: Some(&user), body: views::pages::dashboard(&user) };
    html(StatusCode::OK, views::layout(&state, page))
}

async fn not_found(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    Ok(errors::not_found(&headers, || {
        let page = Page { title: "Not found", path: "", user: user.as_ref(), body: views::pages::not_found() };
        html(StatusCode::NOT_FOUND, views::layout(&state, page))
    }))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn robots(State(state): State<SharedState>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        format!("User-agent: *\nAllow: /\nSitemap: {}/sitemap.xml\n", state.cfg.site_url.trim_end_matches('/')),
    )
        .into_response()
}

async fn sitemap(State(state): State<SharedState>) -> Result<Response, AppError> {
    Ok((
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        state.sitemap.clone(),
    )
        .into_response())
}

#[cfg(feature = "blog")]
fn blog_index(state: &SharedState, user: Option<&User>) -> Markup {
    let page = Page { title: "Latest articles", path: "/blog", user, body: views::blog::index(&state.blog) };
    views::layout_with_metadata(state, page, Some(crate::blog::DESCRIPTION), None)
}

#[cfg(feature = "blog")]
async fn blog_article(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    axum::extract::Path(_slug): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    not_found(State(state), MaybeUser(user), headers).await
}

#[cfg(feature = "blog")]
async fn blog_feed(State(state): State<SharedState>) -> Result<Response, AppError> {
    Ok((
        [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        state.blog.rss.clone(),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[cfg(feature = "auth")]
#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[cfg(feature = "auth")]
pub(crate) fn auth_page(state: &SharedState, kind: views::pages::AuthKind, email: &str, error: Option<&str>) -> Markup {
    let (title, path) = match kind {
        views::pages::AuthKind::SignUp => ("Create your account", "/sign-up"),
        views::pages::AuthKind::SignIn => ("Sign in", "/sign-in"),
    };
    #[cfg(feature = "social-login")]
    let providers = state.social_login.available();
    #[cfg(not(feature = "social-login"))]
    let providers: Vec<(&str, &str)> = Vec::new();
    let body = maud::html! {
        (views::pages::auth(kind, email, error, &providers))
        @if kind == views::pages::AuthKind::SignIn { script defer src=(state.asset_url("passkeys.js")) {} }
    };
    views::layout(state, Page { title, path, user: None, body })
}

#[cfg(feature = "auth")]
async fn sign_up_page(State(state): State<SharedState>, MaybeUser(user): MaybeUser) -> Response {
    if user.is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    html(StatusCode::OK, auth_page(&state, views::pages::AuthKind::SignUp, "", None))
}

#[cfg(feature = "auth")]
async fn sign_in_page(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    // `?{provider}=…` reports how a provider sign-in ended when it did not end in a session.
    Query(notice): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if user.is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    #[cfg(feature = "social-login")]
    let message = crate::social_login::sign_in_notice(&notice);
    #[cfg(not(feature = "social-login"))]
    let message: Option<String> = {
        let _ = notice;
        None
    };
    html(StatusCode::OK, auth_page(&state, views::pages::AuthKind::SignIn, "", message.as_deref()))
}

#[cfg(feature = "auth")]
async fn sign_up(State(state): State<SharedState>, Form(form): Form<Credentials>) -> Result<Response, AppError> {
    let Some(client) = &state.email else {
        return Ok(html(
            StatusCode::SERVICE_UNAVAILABLE,
            auth_page(
                &state,
                views::pages::AuthKind::SignUp,
                &form.email,
                Some("Email delivery is temporarily unavailable. Please try again later."),
            ),
        ));
    };
    match crate::verification::start(&state.pool, client, &form.email, form.password).await {
        Ok(crate::verification::Start::Pending { token, delivered }) => {
            let mut response = if delivered {
                Redirect::to("/verify-email").into_response()
            } else {
                verification_page(
                    &state,
                    StatusCode::SERVICE_UNAVAILABLE,
                    60,
                    Some("We couldn’t send your code. Wait a minute, then try resending."),
                )
            };
            response
                .headers_mut()
                .insert(header::SET_COOKIE, crate::verification::cookie(&token, state.cfg.secure_cookies()));
            Ok(response)
        }
        Ok(crate::verification::Start::Limited) => Ok(html(
            StatusCode::TOO_MANY_REQUESTS,
            auth_page(
                &state,
                views::pages::AuthKind::SignUp,
                &form.email,
                Some("Please wait at least a minute before trying again. Too many attempts? Try again in an hour."),
            ),
        )),
        Err(users::SignUpError::Other(e)) => Err(e.into()),
        Err(e) => Ok(html(
            StatusCode::UNPROCESSABLE_ENTITY,
            auth_page(&state, views::pages::AuthKind::SignUp, &form.email, Some(&e.to_string())),
        )),
    }
}

#[cfg(feature = "auth")]
fn verification_page(state: &SharedState, status: StatusCode, seconds: i64, error: Option<&str>) -> Response {
    let body = maud::html! {
        section class="page auth-page verification" {
            div class="auth-card" {
                header class="auth-head" {
                    h1 { "Check your email" }
                    p class="muted" { "If this email can be registered, we sent a six-character code. Enter it below to finish creating your account. Already registered? " a href="/sign-in" { "Sign in" } "." }
                }
                @if let Some(error) = error { p role="alert" class="alert" { (error) } }
                form class="form auth-form" method="post" action="/verify-email" {
                    label for="code" { "Verification code" }
                    input id="code" name="code" type="text" required maxlength="6" minlength="6" autocomplete="one-time-code" autocapitalize="characters" spellcheck="false" autofocus;
                    span class="hint" { "The code expires in 10 minutes. Check your spam folder too." }
                    button class="btn btn-primary btn-block" type="submit" { "Verify email" }
                }
                form class="auth-alt resend" method="post" action="/verify-email/resend" {
                    button class="btn btn-block" type="submit" data-resend-seconds=(seconds) disabled[seconds > 0] { "Resend code" }
                    p class="hint" data-resend-status aria-live="polite" { @if seconds > 0 { "You can resend in " (seconds) " seconds. Reload this page after the wait if JavaScript is disabled." } }
                }
            }
            p class="muted small auth-switch" { "Wrong email or a different browser? " a href="/sign-up" { "Start again" } }
        }
        script defer src=(state.asset_url("verification.js")) {}
    };
    let mut response = html(
        status,
        views::layout(state, Page { title: "Verify your email", path: "/verify-email", user: None, body }),
    );
    response.headers_mut().insert("x-robots-tag", axum::http::HeaderValue::from_static("noindex, nofollow"));
    response
}
#[cfg(feature = "auth")]
async fn verify_email_page(State(state): State<SharedState>, headers: HeaderMap) -> Result<Response, AppError> {
    let Some(token) = crate::verification::token(&headers) else {
        return Ok(Redirect::to("/sign-up").into_response());
    };
    let seconds = crate::verification::remaining(&state.pool, token).await?;
    Ok(verification_page(&state, StatusCode::OK, seconds, None))
}
#[cfg(feature = "auth")]
#[derive(Deserialize)]
struct VerificationForm {
    code: String,
}
#[cfg(feature = "auth")]
async fn verify_email(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Form(form): Form<VerificationForm>,
) -> Result<Response, AppError> {
    let Some(token) = crate::verification::token(&headers) else {
        return Ok(Redirect::to("/sign-up").into_response());
    };
    let Some(client) = &state.email else {
        return Ok(verification_page(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            0,
            Some("Email verification is temporarily unavailable."),
        ));
    };
    if let Some(user) = crate::verification::verify(&state.pool, client, token, &form.code).await? {
        let mut response = start_session(&state, &user).await?;
        response.headers_mut().append(header::SET_COOKIE, crate::verification::cookie("", state.cfg.secure_cookies()));
        #[cfg(feature = "posthog")]
        crate::analytics::record_user(&user);
        #[cfg(feature = "posthog-product")]
        state.track(&user, "user signed up", serde_json::json!({"method":"email", "email_verified":true}));
        return Ok(response);
    }
    let seconds = crate::verification::remaining(&state.pool, token).await?;
    Ok(verification_page(
        &state,
        StatusCode::UNPROCESSABLE_ENTITY,
        seconds,
        Some("This code is incorrect, expired, or has reached its attempt limit. Request a new code."),
    ))
}
#[cfg(feature = "auth")]
async fn resend_verification(State(state): State<SharedState>, headers: HeaderMap) -> Result<Response, AppError> {
    let Some(token) = crate::verification::token(&headers) else {
        return Ok(Redirect::to("/sign-up").into_response());
    };
    let Some(client) = &state.email else {
        return Ok(verification_page(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            0,
            Some("Email delivery is temporarily unavailable."),
        ));
    };
    let (status, message) = match crate::verification::resend(&state.pool, client, token).await? {
        crate::verification::Start::Pending { delivered: true, .. } => {
            (StatusCode::OK, "A new code has been sent. Only the newest code will work.")
        }
        crate::verification::Start::Pending { delivered: false, .. } => {
            (StatusCode::SERVICE_UNAVAILABLE, "We couldn’t send your code. Wait a minute, then try again.")
        }
        crate::verification::Start::Limited => (
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait at least a minute between requests. Too many attempts? Try again in an hour, or start again if your signup expired.",
        ),
    };
    let seconds = crate::verification::remaining(&state.pool, token).await?;
    Ok(verification_page(&state, status, seconds, Some(message)))
}

/// Auth forms require positive same-origin evidence, including clients without Fetch Metadata.
#[cfg(feature = "auth")]
async fn auth_origin(
    State(state): State<SharedState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    // The sign-in provider and OAuth forms are only routed with their features.
    #[cfg(feature = "social-login")]
    let provider = crate::social_login::is_form_path(request.uri().path());
    #[cfg(not(feature = "social-login"))]
    let provider = false;
    let consent = cfg!(feature = "api") && request.uri().path() == app_auth::oauth_server::AUTHORIZE_PATH;
    if request.method() == axum::http::Method::POST
        && (provider
            || consent
            || matches!(
                request.uri().path(),
                "/sign-up"
                    | "/verify-email"
                    | "/verify-email/resend"
                    | "/passkeys/register/options"
                    | "/passkeys/register"
                    | "/passkeys/sign-in/options"
                    | "/passkeys/sign-in"
                    | crate::two_factor::SIGN_IN_PATH
                    | crate::two_factor::SETTINGS_PATH
            ))
        && request.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok())
            != Some(state.cfg.site_url.trim_end_matches('/'))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(feature = "auth")]
async fn sign_in(State(state): State<SharedState>, Form(form): Form<Credentials>) -> Result<Response, AppError> {
    match users::verify_credentials(&state.pool, &form.email, form.password).await? {
        Some(user) => {
            #[cfg(feature = "posthog")]
            crate::analytics::record_user(&user);
            // With a second factor the password alone earns a challenge, not a session.
            if app_auth::two_factor::is_enabled(&state.pool, user.id).await? {
                return crate::two_factor::start_challenge(&state, &user).await;
            }
            let response = start_session(&state, &user).await?;
            #[cfg(feature = "posthog-product")]
            state.track(&user, "user signed in", serde_json::json!({}));
            Ok(response)
        }
        None => Ok(html(
            StatusCode::UNPROCESSABLE_ENTITY,
            auth_page(&state, views::pages::AuthKind::SignIn, &form.email, Some("Wrong email or password.")),
        )),
    }
}

/// A new session for `user`, as the cookie to set. Every sign-in method ends here.
#[cfg(feature = "auth")]
pub(crate) async fn session_cookie(
    state: &SharedState,
    user: &users::User,
) -> Result<axum::http::HeaderValue, AppError> {
    let ttl = TimeDuration::days(state.cfg.session_ttl_days);
    let token = sessions::create(&state.pool, user.id, ttl).await?;
    Ok(sessions::session_cookie(&token, ttl, state.cfg.secure_cookies()))
}

#[cfg(feature = "auth")]
pub(crate) async fn start_session(state: &SharedState, user: &users::User) -> Result<Response, AppError> {
    let cookie = session_cookie(state, user).await?;
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/dashboard")).into_response())
}

#[cfg(feature = "auth")]
async fn sign_out(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    #[cfg(feature = "posthog")]
    if let Some(user) = &user {
        crate::analytics::record_user(user);
    }
    if let Some(token) = sessions::token_from_headers(&headers) {
        sessions::delete(&state.pool, token).await?;
    }
    #[cfg(feature = "posthog-product")]
    if let Some(user) = &user {
        state.track(user, "user signed out", serde_json::json!({}));
    }
    let _ = user;
    let cookie = sessions::clear_cookie(state.cfg.secure_cookies());
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
}

#[cfg(feature = "docs")]
async fn llms_txt(State(state): State<SharedState>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        state.llms_txt.clone(),
    )
        .into_response()
}

#[cfg(feature = "docs")]
async fn llms_full_txt(State(state): State<SharedState>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        state.llms_full_txt.clone(),
    )
        .into_response()
}

#[cfg(feature = "docs")]
fn docs_index(state: &SharedState, user: Option<&User>) -> Markup {
    let doc = &state.docs.welcome;
    let page = Page { title: &doc.title, path: &doc.path, user, body: views::docs::page(&state.docs, doc) };
    views::layout_with_metadata(state, page, Some(&doc.description), None)
}

#[cfg(feature = "docs")]
async fn docs_article(
    State(state): State<SharedState>,
    MaybeUser(user): MaybeUser,
    axum::extract::Path(slug): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if slug == "welcome" {
        return Ok(Redirect::permanent("/docs").into_response());
    }
    Ok(errors::not_found(&headers, || {
        let page = Page { title: "Not found", path: "", user: user.as_ref(), body: views::pages::not_found() };
        html(StatusCode::NOT_FOUND, views::layout(&state, page))
    }))
}

#[cfg(feature = "stripe")]
fn stripe_client(state: &SharedState) -> Result<&crate::billing::StripeClient, AppError> {
    state
        .stripe
        .as_ref()
        .ok_or_else(|| AppError::Status(StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured yet.".into()))
}
#[cfg(feature = "stripe")]
async fn billing_response(
    state: &SharedState,
    user: &users::User,
    status: StatusCode,
    message: Option<&str>,
) -> Result<Response, AppError> {
    let billing = crate::billing::status(&state.pool, user.id).await?;
    let body = views::pages::billing(&billing, state.stripe.as_ref().map(|s| &s.cfg), message);
    Ok(html(status, views::layout(state, Page { title: "Billing", path: "/billing", user: Some(user), body })))
}
#[cfg(feature = "stripe")]
async fn billing_page(State(state): State<SharedState>, CurrentUser(user): CurrentUser) -> Result<Response, AppError> {
    billing_response(&state, &user, StatusCode::OK, None).await
}
#[cfg(feature = "stripe")]
async fn billing_success(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    billing_response(&state, &user, StatusCode::OK, Some("Thanks! Your billing status updates after Stripe confirms your checkout. If it is still pending, refresh this page shortly.")).await
}
#[cfg(feature = "stripe")]
#[derive(Deserialize)]
struct BillingForm {
    #[serde(default)]
    mode: String,
}
#[cfg(feature = "stripe")]
async fn start_checkout(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    Form(form): Form<BillingForm>,
) -> Result<Response, AppError> {
    use crate::billing::Checkout;
    if user.is_admin {
        return billing_response(&state, &user, StatusCode::OK, None).await;
    }
    let stripe = stripe_client(&state)?;
    let available = match form.mode.as_str() {
        "subscription" => stripe.cfg.subscriptions_enabled(),
        "payment" => stripe.cfg.payments_enabled(),
        _ => false,
    };
    if !available {
        return billing_response(
            &state,
            &user,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("Choose an available billing plan."),
        )
        .await;
    }
    let message = match crate::billing::checkout(&state.pool, stripe, &user, &form.mode, &state.cfg.site_url).await {
        Ok(Checkout::Redirect(url)) => return Ok(Redirect::to(&url).into_response()),
        Ok(Checkout::Pending) => "Your payment is pending. Please wait for confirmation.",
        Ok(Checkout::Retry) => "Your previous checkout has ended. Choose a plan to try again.",
        Ok(Checkout::AdminAccess) => "Your admin role includes all paid features. No payment is required.",
        Ok(Checkout::AlreadyPurchased) => "You already have a purchase or subscription. Manage it below.",
        Err(error) => {
            tracing::error!(%error, "checkout failed");
            return billing_response(
                &state,
                &user,
                StatusCode::BAD_GATEWAY,
                Some("Unable to start checkout. Please try again shortly."),
            )
            .await;
        }
    };
    billing_response(&state, &user, StatusCode::OK, Some(message)).await
}
#[cfg(feature = "stripe")]
async fn billing_portal(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    let url = stripe_client(&state)?.portal(&state.pool, user.id, &state.cfg.site_url).await?;
    Ok(Redirect::to(&url).into_response())
}
#[cfg(feature = "stripe")]
async fn stripe_webhook(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, AppError> {
    let stripe = stripe_client(&state)?;
    let signature = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or_default();
    crate::billing::webhook(&state.pool, stripe, signature, &body).await?;
    Ok(StatusCode::OK)
}

async fn benchmark_data() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], include_str!("../content/site/data.json"))
}
async fn benchmark_methodology() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], include_str!("../content/site/methodology.md"))
}
