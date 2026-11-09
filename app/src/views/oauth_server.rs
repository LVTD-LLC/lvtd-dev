//! The OAuth consent page (`/oauth/authorize`) and its refusal page.

use app_auth::User;
use app_auth::oauth_server::Authorization;
use maud::{Markup, html};

pub fn consent(authorization: &Authorization, user: &User) -> Markup {
    let client = &authorization.client;
    let from = client.origin();
    html! {
        section class="page auth-page oauth-consent" {
            div class="auth-card" {
                header class="auth-head" {
                    h1 { "Connect " (client.name) }
                    p class="muted" {
                        (client.name) " wants to use your " (super::SITE_NAME) " account, "
                        strong { (user.email) } "."
                    }
                }
                ul class="oauth-access" {
                    li { "It will be able to do what you can do: the API and its tools act as your account." }
                    li { "You can disconnect it at any time from " a href="/settings#connected-apps" { "Settings" } ". It also lapses after 30 days without use." }
                }
                @if let Some(from) = from {
                    p class="muted small" { "Registered from " a href=(client.client_uri.as_deref().unwrap_or_default()) rel="noopener noreferrer" { (from) } "." }
                }
                form class="form auth-form" method="post" action="/oauth/authorize" {
                    @for (name, value) in authorization.request.fields() {
                        input type="hidden" name=(name) value=(value);
                    }
                    button class="btn btn-primary btn-block" type="submit" name="decision" value="allow" { "Allow " (client.name) }
                    button class="btn btn-block" type="submit" name="decision" value="deny" { "Cancel" }
                }
            }
            p class="muted small auth-switch" { "Only allow applications you started connecting yourself." }
        }
    }
}

pub fn refused(message: &str) -> Markup {
    html! {
        section class="page auth-page oauth-consent" {
            div class="auth-card" {
                header class="auth-head" {
                    h1 { "Authorization refused" }
                    p class="muted" { (message) }
                }
                p class="muted small" { "Nothing was connected. Go back to the application and try connecting again." }
            }
        }
    }
}
