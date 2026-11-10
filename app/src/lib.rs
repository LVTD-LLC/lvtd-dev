//! The app shell: state, router, pages. This is the crate you edit to build
//! your product; the `lvtd-dev-*` crates underneath are upgraded in place.
//!
//! - [`routes`] axum handlers
//! - [`views`]  maud templates
//! - [`assets`] embedded static files
//! - [`api`]    optional JSON API at `/api/v1`, authenticated with [`api_keys`] or [`oauth_server`] tokens
//! - [`mcp`]    optional MCP server at `/mcp` for coding agents, same credentials
//! - [`oauth_server`]  optional OAuth 2.1 server so hosted agents connect with a sign-in instead of a key
//! - [`jobs`]   background job kinds and their registry
//! - [`avatars`] optional profile pictures: the example of file storage (`app_core::storage`)
//! - [`two_factor`] authenticator-app enrolment and the sign-in code prompt
//! - [`social_login`] optional Google and GitHub sign-in, linking and session-bound confirmation
//!
//! Accounts (sign-up, sign-in, settings, the dashboard, admin) are the `auth`
//! feature; without it the app is a public site and nobody can sign in.
//! - [`vectors`] optional vector search on Qdrant, scoped per user

#[cfg(feature = "posthog")]
pub mod analytics;
#[cfg(any(feature = "posthog", feature = "sentry-server"))]
pub mod observability;
#[cfg(feature = "sentry")]
pub mod sentry;
pub use errors::AppError;
#[cfg(feature = "auth")]
pub mod admin;
#[cfg(feature = "api")]
pub mod api;
#[cfg(feature = "api")]
pub mod api_keys;
pub mod assets;
#[cfg(feature = "storage")]
pub mod avatars;
#[cfg(feature = "stripe")]
pub mod billing;
#[cfg(feature = "blog")]
pub mod blog;
#[cfg(feature = "docs")]
pub mod docs;
mod errors;
pub mod jobs;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "api")]
pub mod oauth_server;
pub mod og;
#[cfg(feature = "auth")]
pub mod passkeys;
pub mod routes;
#[cfg(feature = "auth")]
pub mod settings;
#[cfg(feature = "social-login")]
pub mod social_login;
mod structured_data;
pub mod studio;
#[cfg(feature = "auth")]
pub mod two_factor;
#[cfg(feature = "qdrant")]
pub mod vectors;
#[cfg(feature = "auth")]
pub mod verification;
pub mod views;

use app_auth::AuthState;
#[cfg(feature = "auth")]
use app_auth::RelyingParty;
use app_core::{Config, Pool};
use rust_embed::Embed;
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Embed)]
#[folder = "assets/"]
pub struct Assets;

/// This app's schema: `migrations/` at the workspace root, embedded and
/// applied on connect (`app_core::db::connect`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../migrations");

pub struct AppState {
    /// Script-safe JSON-LD for public pages, prepared at startup.
    pub structured_data: std::collections::HashMap<String, maud::Markup>,
    pub cfg: Config,
    #[cfg(feature = "posthog")]
    pub analytics: Option<analytics::Config>,
    #[cfg(feature = "sentry")]
    pub sentry: Option<sentry::Config>,
    #[cfg(feature = "stripe")]
    pub stripe: Option<billing::StripeClient>,
    /// Qdrant client, `None` until `QDRANT_URL` is configured.
    #[cfg(feature = "qdrant")]
    pub vectors: Option<vectors::Client>,
    #[cfg(feature = "auth")]
    pub email: Option<verification::Client>,
    /// Configured sign-in providers; each stays off until its `{ID}_CLIENT_ID`
    /// and `{ID}_CLIENT_SECRET` are set. See `social_login.rs`.
    #[cfg(feature = "social-login")]
    pub social_login: social_login::Providers,
    /// Where uploads go: an S3-compatible bucket or a local directory.
    #[cfg(feature = "storage")]
    pub storage: app_core::storage::Storage,
    /// WebAuthn relying party (passkeys), derived from `SITE_URL` once.
    #[cfg(feature = "auth")]
    pub rp: RelyingParty,
    /// The OAuth 2.1 authorization server's identity, derived from `SITE_URL` once.
    #[cfg(feature = "api")]
    pub oauth_server: app_auth::oauth_server::Server,
    pub pool: Pool,
    /// Short hash of all embedded assets, used to cache-bust asset URLs.
    pub asset_version: String,
    /// Public sitemap, rendered once at startup and shared across requests.
    pub sitemap: axum::body::Bytes,
    #[cfg(feature = "docs")]
    pub docs: docs::Docs,
    /// `/llms.txt`, a navigation index rendered once at startup.
    #[cfg(feature = "docs")]
    pub llms_txt: axum::body::Bytes,
    /// `/llms-full.txt`, all published guides rendered once at startup.
    #[cfg(feature = "docs")]
    pub llms_full_txt: axum::body::Bytes,
    #[cfg(feature = "blog")]
    pub blog: blog::Blog,
    /// Generated link-preview images for public pages, including the homepage.
    pub og: &'static og::Cards,
}

impl AppState {
    pub fn asset_url(&self, name: &str) -> String {
        format!("/assets/{name}?v={}", self.asset_version)
    }
}

pub type SharedState = Arc<AppState>;

impl AuthState for AppState {
    fn pool(&self) -> &Pool {
        &self.pool
    }
}

/// Build state and router from a config and an already-migrated pool.
/// Used by `main` and by the integration tests.
pub fn build(cfg: Config, pool: Pool) -> (SharedState, axum::Router) {
    build_configured(
        cfg,
        pool,
        #[cfg(feature = "posthog")]
        None,
        #[cfg(feature = "sentry")]
        None,
        #[cfg(feature = "stripe")]
        None,
        #[cfg(feature = "storage")]
        scratch_storage(),
        #[cfg(feature = "qdrant")]
        None,
    )
}

/// Uploads under the system temp directory: what tests and [`build`] use.
/// `main` reads `STORAGE_*` from the environment instead.
#[cfg(feature = "storage")]
pub fn scratch_storage() -> app_core::storage::Storage {
    app_core::storage::Storage::local(std::env::temp_dir().join(concat!(env!("CARGO_PKG_NAME"), "-uploads")))
        .expect("the system temp directory is writable")
}

#[cfg(feature = "posthog")]
pub fn build_with_analytics(
    cfg: Config,
    pool: Pool,
    analytics: Option<analytics::Config>,
) -> (SharedState, axum::Router) {
    build_configured(
        cfg,
        pool,
        analytics,
        #[cfg(feature = "sentry")]
        None,
        #[cfg(feature = "stripe")]
        None,
        #[cfg(feature = "storage")]
        scratch_storage(),
        #[cfg(feature = "qdrant")]
        None,
    )
}

pub fn build_configured(
    cfg: Config,
    pool: Pool,
    #[cfg(feature = "posthog")] analytics: Option<analytics::Config>,
    #[cfg(feature = "sentry")] sentry: Option<sentry::Config>,
    #[cfg(feature = "stripe")] stripe: Option<billing::StripeClient>,
    #[cfg(feature = "storage")] storage: app_core::storage::Storage,
    #[cfg(feature = "qdrant")] vectors: Option<vectors::Client>,
) -> (SharedState, axum::Router) {
    let asset_version = {
        let mut h = Sha256::new();
        for name in Assets::iter() {
            h.update(name.as_bytes());
            if let Some(f) = Assets::get(&name) {
                h.update(&f.data);
            }
        }
        h.finalize()[..6].iter().map(|b| format!("{b:02x}")).collect()
    };
    #[cfg(feature = "blog")]
    let blog = blog::Blog::load(&cfg.site_url).expect("blog content must be valid before serving");
    #[cfg(feature = "blog")]
    let blog_urls = maud::html! {
        url { loc { (cfg.site_url.trim_end_matches('/')) "/blog" } }
        @for post in &blog.posts {
            url { loc { (cfg.site_url.trim_end_matches('/')) (post.path) } }
        }
    };
    #[cfg(not(feature = "blog"))]
    let blog_urls = maud::html! {};
    #[cfg(feature = "docs")]
    let docs = docs::Docs::load().expect("docs content must be valid before serving");
    #[cfg(feature = "docs")]
    let docs_urls = maud::html! {
        url { loc { (cfg.site_url.trim_end_matches('/')) "/docs" } }
        @for doc in &docs.pages { url { loc { (cfg.site_url.trim_end_matches('/')) (doc.path) } } }
    };
    #[cfg(not(feature = "docs"))]
    let docs_urls = maud::html! {};
    #[cfg(feature = "docs")]
    let llms_txt = docs.llms_txt(views::SITE_NAME, &cfg.site_url).expect("docs index must be valid").into();
    #[cfg(feature = "docs")]
    let llms_full_txt = docs.llms_full_txt(views::SITE_NAME, &cfg.site_url).into();
    let sitemap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
        maud::html! {
            urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" {
                (docs_urls)
                url { loc { (cfg.site_url.trim_end_matches('/')) "/" } }
                @for path in &views::company::PATHS {
                    url { loc { (cfg.site_url.trim_end_matches('/')) (path) } }
                }
                @for page in studio::PAGES { url { loc { (cfg.site_url.trim_end_matches('/')) (page.path) } } }
                (blog_urls)
            }
        }
        .into_string()
    )
    .into();
    let og = og::load(
        #[cfg(feature = "blog")]
        &blog,
        #[cfg(feature = "docs")]
        &docs,
    );
    #[cfg(feature = "auth")]
    let rp = RelyingParty::from_site_url(&cfg.site_url, views::SITE_NAME)
        .expect("SITE_URL must be an http(s) origin so passkeys can be bound to it");
    #[cfg(feature = "api")]
    let oauth_server = oauth_server::server(&cfg.site_url);
    let mut state = AppState {
        structured_data: Default::default(),
        cfg,
        #[cfg(feature = "posthog")]
        analytics,
        #[cfg(feature = "sentry")]
        sentry,
        #[cfg(feature = "stripe")]
        stripe,
        #[cfg(feature = "qdrant")]
        vectors,
        pool,
        #[cfg(feature = "auth")]
        email: None,
        #[cfg(feature = "social-login")]
        social_login: Default::default(),
        #[cfg(feature = "storage")]
        storage,
        #[cfg(feature = "auth")]
        rp,
        #[cfg(feature = "api")]
        oauth_server,
        asset_version,
        sitemap,
        #[cfg(feature = "docs")]
        docs,
        #[cfg(feature = "docs")]
        llms_txt,
        #[cfg(feature = "docs")]
        llms_full_txt,
        #[cfg(feature = "blog")]
        blog,
        og,
    };
    state.structured_data = structured_data::build(&state);
    let state = Arc::new(state);
    let router = routes::router(state.clone());
    (state, router)
}

/// The router a worker-only process serves (`JOBS_WORKER=only` or the
/// `worker` command): just the health check the container expects.
pub fn worker_router() -> axum::Router {
    axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(app_core::http::security_headers))
}

/// Only public, published pages; feature-disabled and draft content never enters this list.
pub fn indexnow_paths(state: &SharedState) -> Vec<String> {
    let mut paths = vec!["/".to_string()];
    paths.extend(views::company::PATHS.iter().chain(&views::legal::PATHS).map(|path| path.to_string()));
    #[cfg(feature = "blog")]
    {
        paths.push("/blog".into());
        paths.extend(state.blog.posts.iter().map(|post| post.path.clone()));
    }
    #[cfg(feature = "docs")]
    {
        paths.push("/docs".into());
        paths.extend(state.docs.pages.iter().map(|doc| doc.path.clone()));
    }
    paths.extend(studio::PAGES.iter().map(|page| page.path.to_string()));
    let _ = state; // Both optional content modules can be disabled.
    paths.sort();
    paths.dedup();
    paths
}
