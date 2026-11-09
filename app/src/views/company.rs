//! About and Contact. Search engines and AI assistants read these pages to check that a
//! real business stands behind the product before recommending it, so replace every
//! bracketed placeholder with concrete, true details before launch. Sections about
//! accounts and payments appear only when their Cargo feature is enabled.

use super::SITE_NAME;
use super::legal::{COMPANY, CONTACT_EMAIL};
use maud::{Markup, html};

/// The person or team behind the product, named on the About page and in structured data.
pub const FOUNDER: &str = "[your name]";
/// Where the business is based, e.g. "Austin, Texas, USA".
pub const LOCATION: &str = "[city, country]";
/// Public company pages, listed in the sitemap and IndexNow.
pub const PAGES: &[(&str, &str, &str)] = &[
    (
        "/about",
        "About",
        "Learn who builds and runs this service, why it exists, who it is for and how we treat our customers.",
    ),
    (
        "/contact",
        "Contact",
        "Contact us by email for questions, account help, billing, privacy requests, security reports and partnerships.",
    ),
];
pub const PATHS: [&str; 2] = [PAGES[0].0, PAGES[1].0];

fn email_link() -> Markup {
    html! { a href={ "mailto:" (CONTACT_EMAIL) } { (CONTACT_EMAIL) } }
}

pub fn about() -> Markup {
    html! {
        section class="page legal" {
            h1 { "About " (SITE_NAME) }
            div class="prose" {
                p { (super::SITE_DESCRIPTION) }
                h2 { "Who we are" }
                p {
                    (SITE_NAME) " is built and run by " (FOUNDER) " at " (COMPANY) ", based in " (LOCATION) ". "
                    "[Two or three sentences about your background and why you are the right person to solve this problem.]"
                }
                h2 { "Why it exists" }
                p {
                    "[The problem you kept running into, what people did about it before, and what " (SITE_NAME)
                    " does differently. Concrete details and real numbers are more convincing than adjectives.]"
                }
                h2 { "Who it is for" }
                p {
                    "[The people or businesses " (SITE_NAME) " serves best, and the situations where it is not a good fit.]"
                }
                h2 { "How we work" }
                ul {
                    @if cfg!(feature = "stripe") {
                        li { "Clear pricing: cancel any time from the billing page, and email us about refunds." }
                    }
                    @if cfg!(feature = "auth") {
                        li { "Your account is yours: change your details or delete your account from settings whenever you like." }
                    }
                    li { "We collect only the personal data we need and never sell it. See the " a href="/privacy" { "privacy policy" } "." }
                    li { "A real person reads and answers every message." }
                }
                h2 { "Get in touch" }
                p { "Email " (email_link()) " or see the " a href="/contact" { "contact page" } "." }
            }
        }
    }
}

pub fn contact() -> Markup {
    html! {
        section class="page legal" {
            h1 { "Contact " (SITE_NAME) }
            div class="prose" {
                p {
                    (SITE_NAME) " is run by " (COMPANY) " in " (LOCATION) ". The best way to reach us is email: "
                    (email_link()) ". A real person reads every message, and we aim to reply within "
                    "[two business days]."
                }
                h2 { "What to email us about" }
                ul {
                    li {
                        strong { "Questions. " }
                        "Whether " (SITE_NAME) " fits what you need, how it works, or anything the site does not answer."
                    }
                    @if cfg!(feature = "auth") {
                        li {
                            strong { "Account help. " }
                            "Trouble signing in, verifying your email or using a feature. Write from the address on your "
                            "account so we can find it quickly."
                        }
                    }
                    @if cfg!(feature = "stripe") {
                        li {
                            strong { "Billing and refunds. " }
                            "Questions about a charge, an invoice or a refund. You can manage or cancel your plan yourself "
                            "from the billing page."
                        }
                    }
                    li {
                        strong { "Privacy requests. " }
                        "Access, correct, export or delete your personal data, as described in the "
                        a href="/privacy" { "privacy policy" } "."
                    }
                    li {
                        strong { "Security reports. " }
                        "If you find a vulnerability, email the details privately and we will work with you on a fix "
                        "before anything is disclosed."
                    }
                    li { strong { "Partnerships and press. " } "Integrations, interviews and anything else." }
                }
                h2 { "Policies" }
                p {
                    "The " a href="/terms" { "terms of service" } " and " a href="/privacy" { "privacy policy" }
                    " explain how " (SITE_NAME) " works and how we handle your data."
                }
            }
        }
    }
}
