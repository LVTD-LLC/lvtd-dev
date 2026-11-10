//! HTML templates (maud, compile-time checked). `layout` wraps every page.

pub mod attribution;
pub use attribution::GENERATOR;
#[cfg(feature = "auth")]
pub mod admin;
#[cfg(feature = "api")]
pub mod api_docs;
#[cfg(feature = "blog")]
pub mod blog;
pub mod company;
#[cfg(feature = "docs")]
pub mod docs;
pub mod legal;
#[cfg(feature = "api")]
pub mod oauth_server;
pub mod pages;
#[cfg(feature = "auth")]
pub mod settings;
#[cfg(feature = "auth")]
pub mod two_factor;

use crate::AppState;
use app_auth::User;
use maud::{DOCTYPE, Markup, PreEscaped, html};

pub const SITE_NAME: &str = "LVTD";
pub const HOME_TITLE: &str = "From idea to credible software";
/// Search and link-preview summary for pages that do not pass their own description.
pub const SITE_DESCRIPTION: &str =
    "LVTD scopes, builds, and launches focused MVPs. Fast, finished, and ready for users.";
/// Alt text for `assets/og-image.png`; update both together (see app/og-image/).
const OG_IMAGE_ALT: &str = "Ship your SaaS in Rust.";

/// `08 Oct 2026`, for account and key listings.
pub fn short_date(t: time::OffsetDateTime) -> String {
    t.format(time::macros::format_description!("[day] [month repr:short] [year]")).unwrap_or_default()
}

pub struct Page<'a> {
    pub title: &'a str,
    /// Path (starting with `/`) used for the canonical URL; empty to omit.
    pub path: &'a str,
    pub user: Option<&'a User>,
    pub body: Markup,
}

pub fn layout(state: &AppState, page: Page<'_>) -> Markup {
    layout_with_metadata(state, page, None, None)
}

/// Pages may pass their own description (else `SITE_DESCRIPTION`); blog posts add article metadata.
pub fn layout_with_metadata(
    state: &AppState,
    page: Page<'_>,
    description: Option<&str>,
    article: Option<(&str, time::Date)>,
) -> Markup {
    let full_title = if page.path == "/" {
        format!("{SITE_NAME} — {}", page.title)
    } else {
        format!("{} — {SITE_NAME}", page.title)
    };
    let description = description
        .or_else(|| legal::PAGES.iter().chain(company::PAGES).find(|entry| entry.0 == page.path).map(|entry| entry.2))
        .unwrap_or(SITE_DESCRIPTION);
    let site_url = state.cfg.site_url.trim_end_matches('/');
    let (image, image_alt) = og_image(state, page.path);
    let structured_data = state.structured_data.get(page.path);
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="generator" content=(GENERATOR);
                title { (full_title) }
                meta name="robots" content=(if structured_data.is_some() { "index, follow, max-image-preview:large" } else { "noindex, follow" });
                @if let Some(data) = structured_data {
                    (data)
                }
                @if !page.path.is_empty() {
                    link rel="canonical" href={ (site_url) (page.path) };
                }
                meta name="description" content=(description);
                meta property="og:title" content=(full_title);
                meta property="og:description" content=(description);
                meta property="og:site_name" content=(SITE_NAME);
                @if !page.path.is_empty() {
                    meta property="og:url" content={ (site_url) (page.path) };
                }
                meta property="og:type" content=(if article.is_some() { "article" } else { "website" });
                // Public pages use generated cards; unregistered pages fall back to og-image.png.
                meta property="og:image" content={ (site_url) (image) };
                meta property="og:image:type" content="image/png";
                meta property="og:image:width" content="1200";
                meta property="og:image:height" content="630";
                meta property="og:image:alt" content=(image_alt);
                meta name="twitter:card" content="summary_large_image";
                meta name="twitter:title" content=(full_title);
                meta name="twitter:description" content=(description);
                meta name="twitter:image" content={ (site_url) (image) };
                meta name="twitter:image:alt" content=(image_alt);
                @if let Some((author, date)) = article {
                    meta name="author" content=(author);
                    meta property="article:published_time" content={ (date) "T00:00:00Z" };
                }
                @if cfg!(feature = "blog") {
                    link rel="alternate" type="application/rss+xml" title={ (SITE_NAME) " Blog" } href="/blog/rss.xml";
                }
                link rel="icon" href=(state.asset_url("favicon.svg")) type="image/svg+xml";
                link rel="stylesheet" href=(state.asset_url("style.css"));
                // Apply the saved theme before first paint to avoid a flash.
                script { (PreEscaped(THEME_BOOT)) }
                (analytics_script(state, page.user))
                (sentry_script(state))
            }
            body {
                (navbar(state, &page))
                main class="container" { (page.body) }
                footer class="site-footer" {
                    div class="container footer-inner" {
                        span class="muted" {
                            "© " (legal::COMPANY) ". "
                            // Optional attribution: remove this link if you prefer.
                            a href=(attribution::URL) { (attribution::CREDIT) }
                        }
                        nav aria-label="Footer" {
                            @if cfg!(feature = "docs") { a class="muted" href="/docs" { "Docs" } }
                            @if cfg!(feature = "blog") {
                                a class="muted" href="/blog" { "Blog" }
                                a class="muted" href="/blog/rss.xml" { "RSS" }
                            }
                            a class="muted" href="/ai-steering" { "AI Steering" }
                            a class="muted" href="/services/hosted-openclaw/" { "OpenClaw" }
                            a class="muted" href="/about" { "About" }
                            a class="muted" href="/contact" { "Contact" }
                            a class="muted" href="/terms" { "Terms" }
                            a class="muted" href="/privacy" { "Privacy" }
                            a class="muted" href="/healthz" { "Status" }
                        }
                    }
                }
                script { (PreEscaped(THEME_TOGGLE)) }
            }
        }
    }
}

/// The page's generated card when it has one, else the static default image.
fn og_image<'a>(state: &'a AppState, path: &str) -> (String, &'a str) {
    if let Some(card) = state.og.image(path) {
        return card;
    }
    (state.asset_url("og-image.png"), OG_IMAGE_ALT)
}

const THEME_BOOT: &str = r#"(function(){var t;try{t=localStorage.getItem('theme')}catch(e){}if(t==='dark'||(t!=='light'&&matchMedia('(prefers-color-scheme: dark)').matches))document.documentElement.dataset.theme='dark';})();"#;

const THEME_TOGGLE: &str = r#"(function(){
var media=matchMedia('(prefers-color-scheme: dark)'),preference='system';
try{var saved=localStorage.getItem('theme');if(saved==='light'||saved==='dark')preference=saved}catch(e){}
function apply(){document.documentElement.dataset.theme=preference==='system'?(media.matches?'dark':'light'):preference;document.querySelectorAll('[data-theme-value]').forEach(function(button){button.setAttribute('aria-pressed',String(button.dataset.themeValue===preference))})}
function choose(value){preference=value;try{if(value==='system')localStorage.removeItem('theme');else localStorage.setItem('theme',value)}catch(e){}apply()}
document.querySelectorAll('[data-theme-value]').forEach(function(button){button.addEventListener('click',function(){choose(button.dataset.themeValue)})});
media.addEventListener('change',apply);apply();
document.addEventListener('keydown',function(e){if(e.key==='Escape')document.querySelectorAll('.account-menu[open]').forEach(function(menu){menu.removeAttribute('open');menu.querySelector('summary').focus()})});
document.addEventListener('click',function(e){document.querySelectorAll('.account-menu[open]').forEach(function(menu){if(!menu.contains(e.target))menu.removeAttribute('open')})});
})();"#;

pub fn theme_choices() -> Markup {
    html! {
        div class="theme-choices" role="group" aria-label="Color theme" {
            button type="button" data-theme-value="light" aria-pressed="false" { (sun_icon()) "Light" }
            button type="button" data-theme-value="dark" aria-pressed="false" { (moon_icon()) "Dark" }
            button type="button" data-theme-value="system" aria-pressed="true" {
                svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true" {
                    rect x="3" y="4" width="18" height="12" rx="2" {} path d="M8 20h8m-4-4v4" {}
                }
                "System"
            }
        }
    }
}

/// The uploaded picture when there is one, else the email's first letter.
#[cfg(feature = "auth")]
fn account_avatar(state: &AppState, user: &User) -> Markup {
    #[cfg(feature = "storage")]
    if let Some(key) = &user.avatar_key {
        return html! {
            img class="account-avatar" src=(crate::avatars::url(&state.storage, key)) alt="" width="30" height="30";
        };
    }
    let _ = state;
    html! { span class="account-avatar" aria-hidden="true" { (settings::initial(user)) } }
}

fn navbar(state: &AppState, page: &Page<'_>) -> Markup {
    let dashboard_active = page.path == "/dashboard";
    html! {
        header class=(if page.user.is_some() { "site-header nav-account" } else { "site-header" }) {
            div class="container header-inner" {
                a class="brand" href="/" { (logo()) span { (SITE_NAME) } }
                @if page.user.is_some() {
                    nav class="primary-nav" aria-label="Main navigation" {
                        a class="nav-item" href="/dashboard" aria-current=[dashboard_active.then_some("page")] { "Dashboard" }
                    }
                }
                div class="header-actions" {
                    nav class="resource-nav" aria-label="Resources" {
                        a class="nav-link" href="/projects/" { "Projects" }
                        a class="nav-link" href="/research/" { "Research" }
                        @if cfg!(feature = "docs") { a class="nav-link" href="/docs" { "Docs" } }
                        @if cfg!(feature = "blog") { a class="nav-link" href="/blog" { "Blog" } }
                    }
                    (account_actions(state, page))
                }
            }
        }
    }
}

/// The account menu when signed in, else sign-in and sign-up buttons.
#[cfg(feature = "auth")]
fn account_actions(state: &AppState, page: &Page<'_>) -> Markup {
    html! {
        @match page.user {
            Some(user) => {
                details class="account-menu" {
                    summary aria-label="Account menu" {
                        (account_avatar(state, user))
                        svg class="account-chevron" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true" { path d="m4 6 4 4 4-4" {} }
                    }
                    div class="account-dropdown" {
                        div class="account-identity" { span { "Signed in as" } strong { (user.email) } }
                        @if user.is_admin {
                            a href="/admin" aria-current=[(page.path == "/admin").then_some("page")] { "Admin panel" }
                        }
                        a href="/settings" aria-current=[(page.path == "/settings").then_some("page")] { "Account settings" }
                        form method="post" action="/sign-out" { button type="submit" { "Sign out" } }
                    }
                }
            }
            None => {
                a class="btn" href="/sign-in" { "Sign in" }
                a class="btn btn-primary" href="/sign-up" { "Get started" }
            }
        }
    }
}

/// Without accounts there is nobody to sign in.
#[cfg(not(feature = "auth"))]
fn account_actions(_: &AppState, _: &Page<'_>) -> Markup {
    html! {}
}

pub fn logo() -> Markup {
    html! { svg class="icon brand-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
        path d="M3 17h18l-2 4H5l-2-4z" {}
        path d="M5 17V9l7-5 7 5v8" {}
        path d="M12 4v13" {}
    } }
}
pub fn sun_icon() -> Markup {
    html! { svg class="icon icon-sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true" { circle cx="12" cy="12" r="4" {} path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" {} } }
}
pub fn moon_icon() -> Markup {
    html! { svg class="icon icon-moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" {} } }
}

fn analytics_script(state: &AppState, user: Option<&User>) -> Markup {
    #[cfg(feature = "posthog-web")]
    if let Some(config) = &state.analytics {
        return html! {
            script defer src=(state.asset_url("analytics.js"))
                data-project-token=(config.project_token)
                data-api-host=(config.api_host) data-sdk-url=(config.sdk_url)
                data-web-analytics=(cfg!(feature = "posthog-web"))
                data-user-id=(user.map(|user| user.id.to_string()).unwrap_or_default()) {}
        };
    }
    let _ = (state, user);
    html! {}
}

fn sentry_script(state: &AppState) -> Markup {
    #[cfg(feature = "sentry")]
    if let Some(sentry) = &state.sentry {
        return html! {
            @for (name, value) in crate::sentry::trace_headers() {
                meta name=(name) content=(value);
            }
            script defer src=(state.asset_url("sentry.js"))
                data-errors=(cfg!(feature = "sentry-errors"))
                data-traces=(cfg!(feature = "sentry-traces"))
                data-logs=(cfg!(feature = "sentry-logs"))
                data-metrics=(cfg!(feature = "sentry-metrics"))
                data-replay=(cfg!(feature = "sentry-replay"))
                data-dsn=(sentry.dsn)
                data-environment=(sentry.environment)
                data-release=(crate::sentry::release())
                data-traces-rate=(sentry.traces_sample_rate)
                data-replay-rate=(sentry.replay_sample_rate)
                data-profile-rate=(sentry.profile_sample_rate) {}
        };
    }
    let _ = state;
    html! {}
}
