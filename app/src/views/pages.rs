#[cfg(feature = "auth")]
use app_auth::User;
use maud::{Markup, html};
#[cfg(feature = "auth")]
use time::macros::format_description;

/// Placeholder homepage. Replace the copy, keep the shape: one idea per
/// screen, headlines in plain words (AGENTS.md, "Landing page").
pub fn home() -> Markup {
    html! {
        section class="hero screen" {
            h1 { "Say what it does." br; span class="screen-title-soft" { "In plain words." } }
            p class="lede" {
                "Replace this with one sentence your mum would understand: "
                "who LVTD is for and what it does for them. Keep it under twenty words."
            }
            div class="cta" {
                @if cfg!(feature = "auth") {
                    a class="btn btn-primary btn-lg" href="/sign-up" { "Create an account" }
                    a class="btn btn-lg" href="/sign-in" { "Sign in" }
                } @else {
                    (contact_button())
                }
            }
        }
        section class="screen" aria-labelledby="idea-title" {
            h2 class="screen-title" id="idea-title" { "One idea." br; span class="screen-title-soft" { "One screen." } }
            p class="screen-sub" {
                "Give each benefit its own screen, like posts in a feed. "
                "Lead with what your customer gets, not how it is built, and cut anything a visitor would skim past."
            }
        }
        section class="screen" aria-labelledby="proof-title" {
            h2 class="screen-title" id="proof-title" { "Use a real number." br; span class="screen-title-soft" { "Then prove it." } }
            p class="screen-sub" {
                "\"Saves 3 hours a week\" beats \"boosts productivity\". "
                "Use a figure you measured, and say how you measured it in small print underneath."
            }
        }
        section class="screen" aria-labelledby="start-title" {
            h2 class="screen-title" id="start-title" { "Ask for one thing." }
            p class="screen-sub" {
                "End with the single action you want visitors to take, "
                "using the same words as the button at the top of the page."
            }
            div class="cta" {
                @if cfg!(feature = "auth") {
                    a class="btn btn-primary btn-lg" href="/sign-up" { "Create an account" }
                } @else {
                    (contact_button())
                }
            }
        }
    }
}

/// The call to action of a site without accounts.
fn contact_button() -> Markup {
    html! { a class="btn btn-primary btn-lg" href={ "mailto:" (super::legal::CONTACT_EMAIL) } { "Get in touch" } }
}

#[cfg(feature = "auth")]
pub fn dashboard(user: &User) -> Markup {
    let billing_enabled = cfg!(feature = "stripe");
    let joined = user.created_at.format(format_description!("[day] [month repr:long] [year]")).unwrap_or_default();
    html! {
        section class="page" {
            h1 { "Dashboard" }
            @if billing_enabled { a class="btn" href="/billing" { "Billing" } }
            p class="lede" { "You're signed in as " strong { (user.email) } "." }
            div class="panel" {
                dl {
                    dt { "Email" } dd { (user.email) }
                    dt { "Member since" } dd { (joined) }
                    dt { "User ID" } dd { code { (user.id) } }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "auth")]
pub enum AuthKind {
    SignUp,
    SignIn,
}

/// `providers` are the configured sign-in providers as `(id, name)`, each shown
/// as a button below the email form (`google` and `github` features).
#[cfg(feature = "auth")]
pub fn auth(kind: AuthKind, email: &str, error: Option<&str>, providers: &[(&str, &str)]) -> Markup {
    let continue_with = format!(
        "Sign up with your email, or continue with {}.",
        providers.iter().map(|(_, name)| *name).collect::<Vec<_>>().join(" or ")
    );
    let (heading, action, button, autocomplete, email_autocomplete) = match kind {
        AuthKind::SignUp => ("Create your account", "/sign-up", "Create account", "new-password", "email"),
        // `webauthn` lets the browser offer saved passkeys in the email field's autofill.
        AuthKind::SignIn => ("Welcome back", "/sign-in", "Sign in", "current-password", "username webauthn"),
    };
    let (subtitle, switch_prompt, switch_href, switch_label) = match kind {
        AuthKind::SignUp if !providers.is_empty() => {
            (continue_with.as_str(), "Already have an account?", "/sign-in", "Sign in")
        }
        AuthKind::SignUp => {
            ("Sign up with your email and a password.", "Already have an account?", "/sign-in", "Sign in")
        }
        AuthKind::SignIn => ("Sign in to pick up where you left off.", "New here?", "/sign-up", "Create an account"),
    };
    html! {
        section class="page auth-page" {
            div class="auth-card" {
                header class="auth-head" {
                    h1 { (heading) }
                    p class="muted" { (subtitle) }
                }
                @if let Some(e) = error { p class="alert" role="alert" { (e) } }
                form class="form auth-form" method="post" action=(action) {
                    label { "Email"
                        input type="email" name="email" value=(email) required autocomplete=(email_autocomplete) autofocus placeholder="you@example.com";
                    }
                    label { "Password"
                        input type="password" name="password" required minlength="8" autocomplete=(autocomplete);
                        @if kind == AuthKind::SignUp { span class="hint" { "At least 8 characters." } }
                    }
                    button class="btn btn-primary btn-block" type="submit" { (button) }
                }
                @if !providers.is_empty() {
                    div class="auth-divider" { span { "or" } }
                    @for (id, name) in providers {
                        form class="auth-alt" method="post" action={ "/auth/" (id) } {
                            button class={ "btn btn-block " (id) "-button" } type="submit" { (provider_logo(id)) "Continue with " (name) }
                        }
                    }
                }
                @if kind == AuthKind::SignIn {
                    // Hidden until passkeys.js confirms the browser supports WebAuthn.
                    div class="auth-alt passkey-signin" hidden data-passkey-signin data-options-url="/passkeys/sign-in/options" data-sign-in-url="/passkeys/sign-in" {
                        @if providers.is_empty() { div class="auth-divider" { span { "or" } } }
                        button class="btn btn-block" type="button" data-passkey-button { (key_icon()) "Sign in with a passkey" }
                        p class="hint" data-passkey-status role="status" aria-live="polite" {}
                    }
                }
            }
            p class="muted small auth-switch" { (switch_prompt) " " a href=(switch_href) { (switch_label) } }
        }
    }
}

#[cfg(feature = "auth")]
fn provider_logo(id: &str) -> Markup {
    match id {
        "google" => google_logo(),
        "github" => github_logo(),
        _ => html! {},
    }
}

/// Google's standard "G" mark, drawn per its sign-in branding guidelines.
#[cfg(feature = "auth")]
fn google_logo() -> Markup {
    html! { svg class="google-logo" viewBox="0 0 48 48" aria-hidden="true" {
        path fill="#EA4335" d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z" {}
        path fill="#4285F4" d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z" {}
        path fill="#FBBC05" d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z" {}
        path fill="#34A853" d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z" {}
    } }
}

/// GitHub's Invertocat mark (Octicons `mark-github`), drawn in the text colour.
#[cfg(feature = "auth")]
fn github_logo() -> Markup {
    html! { svg class="github-logo" viewBox="0 0 16 16" aria-hidden="true" {
        path fill="currentColor" d="M8 0c4.42 0 8 3.58 8 8a8.013 8.013 0 0 1-5.45 7.59c-.4.08-.55-.17-.55-.38 0-.27.01-1.13.01-2.2 0-.75-.25-1.23-.54-1.48 1.78-.2 3.65-.88 3.65-3.95 0-.88-.31-1.59-.82-2.15.08-.2.36-1.02-.08-2.12 0 0-.67-.22-2.2.82-.64-.18-1.32-.27-2-.27-.68 0-1.36.09-2 .27-1.53-1.03-2.2-.82-2.2-.82-.44 1.1-.16 1.92-.08 2.12-.51.56-.82 1.28-.82 2.15 0 3.06 1.86 3.75 3.64 3.95-.23.2-.44.55-.51 1.07-.46.21-1.61.55-2.33-.66-.15-.24-.6-.83-1.23-.82-.67.01-.27.38.01.53.34.19.73.9.82 1.13.16.45.68 1.31 2.69.94 0 .67.01 1.3.01 1.49 0 .21-.15.45-.55.38A7.995 7.995 0 0 1 0 8c0-4.42 3.58-8 8-8Z" {}
    } }
}

#[cfg(feature = "auth")]
fn key_icon() -> Markup {
    html! { svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
        circle cx="7.5" cy="15.5" r="4.5" {}
        path d="M10.8 12.2 21 2m-3 3 3 3m-6 0 3 3" {}
    } }
}

pub fn not_found() -> Markup {
    html! {
        section class="page" {
            h1 { "Page not found" }
            p class="lede" { "There's nothing at this address." }
            a class="btn" href="/" { "Back home" }
        }
    }
}

#[cfg(feature = "stripe")]
pub fn billing(
    status: &crate::billing::BillingStatus,
    config: Option<&crate::billing::Config>,
    message: Option<&str>,
) -> Markup {
    html! {
        section class="page narrow" {
            h1 { "Billing" }
            @if let Some(message) = message { p class="alert" role="status" { (message) } }
            @if status.is_admin {
                p { strong { "Admin access" } }
                p { "Your admin role includes all paid features. No payment is required." }
                @if status.subscription_status.as_deref().is_some_and(|s| !matches!(s, "canceled" | "incomplete_expired")) {
                    p class="muted" { "Your existing subscription is unchanged. Use Manage billing to cancel it if you no longer need it." }
                }
            }
            @if status.paid { p { "Your one-time purchase is confirmed." } }
            @if let Some(subscription) = &status.subscription_status {
                p { "Subscription: " strong { (subscription) } }
                @if status.cancel_at_period_end { p { "Your subscription will end at the end of the current billing period." } }
            }
            @if let Some(config) = config {
                @if !status.is_admin && !status.paid && !status.subscription_status.as_deref().is_some_and(|s| !matches!(s, "canceled" | "incomplete_expired")) {
                    p class="lede" { "Choose a plan. Review the price and payment details securely on Stripe." }
                    form class="form" method="post" action="/billing" {
                        @if config.subscriptions_enabled() { button class="btn btn-primary" name="mode" value="subscription" type="submit" { "Subscribe" } }
                        @if config.payments_enabled() { button class="btn" name="mode" value="payment" type="submit" { "Pay once" } }
                    }
                }
                @if status.has_customer {
                    form method="post" action="/billing/portal" { button class="btn" type="submit" { "Manage billing" } }
                    p class="muted small" { "Update your payment method, view invoices, or cancel your subscription on Stripe." }
                }
            } @else if !status.is_admin { p { "Billing is not configured yet." } }
            a href="/dashboard" { "Back to dashboard" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homepage_content_and_headings_are_readable_without_javascript() {
        // This fragment is the homepage's main content, rendered without scripts.
        // Inspect Maud's generated tags and text without adding a parser dependency.
        let html = home().into_string();
        let mut in_paragraph = false;
        let mut prose = String::new();
        let mut previous = 0;
        let mut h1_count = 0;
        for element in html.split('<').skip(1) {
            let (tag, text) = element.split_once('>').unwrap();
            let tag = tag.split_ascii_whitespace().next().unwrap();
            match tag {
                "p" => in_paragraph = true,
                "/p" => {
                    in_paragraph = false;
                    prose.push(' ');
                }
                _ => {}
            }
            if in_paragraph {
                prose.push_str(text);
            }
            if let [b'h', level @ b'1'..=b'6'] = tag.as_bytes() {
                let level = level - b'0';
                assert!(level <= previous + 1, "heading hierarchy skips H{previous} to H{level}");
                assert!(!text.trim().is_empty(), "headings must have text");
                h1_count += usize::from(level == 1);
                previous = level;
            }
        }
        assert_eq!(h1_count, 1, "homepage needs exactly one H1");
        let prose = prose.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(prose.chars().count() >= 500, "homepage needs at least 500 characters of meaningful prose");
    }

    #[cfg(feature = "auth")]
    #[test]
    fn auth_pages_put_google_below_the_email_form() {
        for kind in [AuthKind::SignUp, AuthKind::SignIn] {
            let page = auth(kind, "", None, &[("google", "Google")]).into_string();
            let email = page.find("type=\"email\"").unwrap();
            let google = page.find("action=\"/auth/google\"").unwrap();
            assert!(email < google, "Google must follow the email form");
            assert!(page.contains("Continue with Google") && page.contains("class=\"google-logo\""));
            if kind == AuthKind::SignUp {
                assert!(page.contains("or continue with Google"));
            }
            let without = auth(kind, "", None, &[]).into_string();
            assert!(!without.contains("/auth/google") && !without.contains("Google"));
        }
    }
}
