//! Terms of Service and Privacy Policy. A starting point, not legal advice: fill in the
//! constants below, read both pages, and have them reviewed before launch. Third-party
//! services appear only when their Cargo feature is enabled.

use maud::{Markup, html};

/// The legal entity that operates the app. Defaults to the product name.
pub const COMPANY: &str = "LVTD, LLC";
/// Where people send legal, privacy and refund requests.
pub const CONTACT_EMAIL: &str = "rasul@lvtd.dev";
/// Whose law governs the terms, e.g. "the State of Delaware, USA".
pub const GOVERNING_LAW: &str = "[your state or country]";
/// Change whenever the substance of either page changes.
pub const UPDATED: &str = "[date]";
/// Public legal pages, listed in the sitemap and IndexNow.
pub const PAGES: &[(&str, &str, &str)] = &[
    (
        "/terms",
        "Terms of Service",
        "Read the terms for using this service, including account responsibilities, acceptable use and limitations.",
    ),
    (
        "/privacy",
        "Privacy Policy",
        "Learn how this service collects, uses and protects personal information, and how to contact us about your data.",
    ),
];
pub const PATHS: [&str; 2] = [PAGES[0].0, PAGES[1].0];

/// The sign-in providers this build offers, as prose: "Google or GitHub".
fn providers() -> String {
    let enabled = [(cfg!(feature = "google"), "Google"), (cfg!(feature = "github"), "GitHub")];
    enabled.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect::<Vec<_>>().join(" or ")
}

fn legal_page(title: &str, intro: Markup, body: Markup) -> Markup {
    html! {
        section class="page legal" {
            h1 { (title) }
            p class="muted small legal-updated" { "Last updated " (UPDATED) }
            div class="prose" {
                p { (intro) }
                (body)
                h2 { "Contact" }
                p { "Questions about this page? Email " a href={ "mailto:" (CONTACT_EMAIL) } { (CONTACT_EMAIL) } "." }
            }
        }
    }
}

pub fn terms() -> Markup {
    legal_page(
        "Terms of Service",
        html! {
            "These terms govern your use of " (super::SITE_NAME) ", operated by " (COMPANY)
            " (\"we\", \"us\"). "
            @if cfg!(feature = "auth") { "By creating an account you agree to them." } @else { "By using it you agree to them." }
        },
        html! {
            @if cfg!(feature = "auth") {
                h2 { "Your account" }
                p {
                    "You need a verified email address to create an account. Keep your credentials safe: you are responsible "
                    "for activity under your account. You must be at least 16 years old, or the age of digital consent where you live."
                }
            }
            @if cfg!(feature = "stripe") {
                h2 { "Payments" }
                p {
                    "Paid plans are billed in advance through Stripe. Subscriptions renew automatically until you cancel "
                    "them from the billing page; cancelling stops future charges and keeps access until the end of the "
                    "period you paid for. For refunds, email " a href={ "mailto:" (CONTACT_EMAIL) } { (CONTACT_EMAIL) } "."
                }
            }
            h2 { "Your content" }
            p {
                "You keep ownership of what you put into " (super::SITE_NAME) ". You give us permission to store and "
                "process it only as needed to run the service for you. Do not upload anything you do not have the right to share."
            }
            h2 { "Acceptable use" }
            p {
                "Do not use the service to break the law, harm others, attack or overload our infrastructure, or get around "
                "access limits. "
                @if cfg!(feature = "auth") { "We may suspend or close accounts that do." } @else { "We may block access for anyone who does." }
            }
            @if cfg!(feature = "auth") {
                h2 { "Ending your account" }
                p {
                    "You can delete your account at any time from the settings page. We may suspend or end access if you breach "
                    "these terms, and will tell you why unless the law prevents it."
                }
            }
            h2 { "No warranty" }
            p {
                "The service is provided \"as is\" and \"as available\". We work to keep it reliable and secure, but we do "
                "not promise it will be uninterrupted or error-free."
            }
            h2 { "Limitation of liability" }
            p {
                "To the extent the law allows, " (COMPANY) " is not liable for indirect, incidental or consequential "
                "damages, or lost profits or data. Our total liability for any claim is limited to the amount you paid us "
                "in the 12 months before it."
            }
            h2 { "Changes" }
            p {
                "We may update these terms. "
                @if cfg!(feature = "auth") { "If a change is material we will tell account holders before it takes effect. " }
                "Continuing to use the service after that means you accept the new terms."
            }
            h2 { "Governing law" }
            p { "These terms are governed by the laws of " (GOVERNING_LAW) ", without regard to conflict-of-law rules." }
        },
    )
}

pub fn privacy() -> Markup {
    legal_page(
        "Privacy Policy",
        html! {
            "This policy explains what personal data " (COMPANY) " collects when you use " (super::SITE_NAME)
            ", why, and what you can do about it. We collect only what we need to run the service and never sell it."
        },
        html! {
            h2 { "What we collect" }
            ul {
                @if cfg!(feature = "auth") {
                    li {
                        strong { "Account data. " }
                        "Your email address, a hash of your password (never the password itself), your optional name"
                        @if cfg!(feature = "storage") { " and profile picture" }
                        ", and, if you enable them, passkey public keys and an authenticator-app secret."
                        @if cfg!(feature = "social-login") {
                            " If you sign in with " (providers()) ", we store that account's identifier and receive your email address and whether it has been verified. Accounts created this way have no password."
                        }
                    }
                }
                @if cfg!(feature = "api") {
                    li { strong { "API keys. " } "Hashes of personal API keys you create, and when they were last used." }
                }
                @if cfg!(feature = "stripe") {
                    li {
                        strong { "Payments. " }
                        "Stripe handles your card details; we store your Stripe customer reference and subscription status."
                    }
                }
                @if cfg!(feature = "auth") {
                    li {
                        strong { "Sessions and security. " }
                        "A session cookie that keeps you signed in, and short-lived hashed request identifiers used to limit abuse."
                    }
                } @else {
                    li { strong { "Server logs. " } "Your IP address and the pages you request, kept briefly to run and secure the service." }
                }
                @if cfg!(feature = "posthog") || cfg!(feature = "sentry") {
                    li {
                        strong { "Usage and errors. " }
                        "Pages viewed, product events, performance metrics and error reports"
                        @if cfg!(feature = "sentry-replay") { ", and session replays with text and inputs masked" }
                        "."
                    }
                }
            }
            h2 { "Who processes it" }
            p { "We share data only with the services that run " (super::SITE_NAME) ":" }
            ul {
                @if cfg!(feature = "auth") { li { strong { "Mailgun" } " to send verification and account emails." } }
                @if cfg!(feature = "google") { li { strong { "Google" } " to verify your identity when you choose to sign in with Google. We request only your identity and email address, and keep no Google access or refresh tokens." } }
                @if cfg!(feature = "github") { li { strong { "GitHub" } " to verify your identity when you choose to sign in with GitHub. We request only your account identifier and email addresses, and revoke GitHub's access token immediately after reading them." } }
                @if cfg!(feature = "stripe") { li { strong { "Stripe" } " for payments." } }
                @if cfg!(feature = "posthog") { li { strong { "PostHog" } " for product analytics." } }
                @if cfg!(feature = "sentry") { li { strong { "Sentry" } " for error tracking and performance monitoring." } }
                @if cfg!(feature = "storage") { li { "Our file storage provider, to store uploaded files." } }
                @if cfg!(feature = "api") { li { strong { "jsDelivr" } " to deliver the code of the interactive API reference, which sees your IP address when you open it." } }
                li { "Our hosting provider, which runs our servers and database." }
            }
            h2 { "Cookies and local storage" }
            p {
                @if cfg!(feature = "social-login") { "We use essential cookies for your session and for the few minutes a " (providers()) " sign-in is in progress. " }
                @else if cfg!(feature = "auth") { "We use one essential session cookie. " } @else { "We set no cookies of our own. " }
                "Your light or dark theme choice is saved in your browser's local storage."
                @if cfg!(feature = "posthog") || cfg!(feature = "sentry-replay") {
                    " Analytics and monitoring store an anonymous identifier in your browser."
                }
                " We do not use advertising cookies."
            }
            h2 { "How long we keep it" }
            p {
                @if cfg!(feature = "auth") {
                    "Account data stays until you delete your account. Unfinished sign-ups and security challenges expire "
                    "within hours. "
                }
                "Records we must keep for legal or tax reasons are kept as long as the law requires."
            }
            h2 { "Your rights" }
            p {
                @if cfg!(feature = "auth") {
                    "You can update your profile and password on the settings page, and delete your account there at any time. "
                    "You can also email us"
                } @else { "You can email us" }
                " to access, correct, export or erase your data. Depending on where you live (for "
                "example the EU, UK or California), you may have further rights, including complaining to your data "
                "protection authority."
            }
            h2 { "Children" }
            p { (super::SITE_NAME) " is not directed at children under 16, and we do not knowingly collect their data." }
            h2 { "Changes" }
            p {
                "If we change this policy materially, we will "
                @if cfg!(feature = "auth") { "tell account holders and " }
                "update the date above."
            }
        },
    )
}
