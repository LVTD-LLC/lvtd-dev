#[cfg(feature = "api")]
use crate::api_keys;
use crate::settings::Profile;
use app_auth::User;
use app_auth::passkeys::Passkey;
use maud::{Markup, html};
use time::macros::format_description;

pub use crate::settings::View;

pub fn page(profile: &Profile, view: &View<'_>, user: &User) -> Markup {
    let is_admin = user.is_admin;
    let mut account = vec![("profile", "Profile")];
    if cfg!(feature = "storage") {
        account.push(("avatar", "Profile picture"));
    }
    account.push(("appearance", "Appearance"));
    let mut security = sign_in_toc(view);
    security.extend([("passkeys", "Passkeys".to_owned()), ("two-factor", "Two-factor".to_owned())]);
    let security: Vec<(&str, &str)> = security.iter().map(|(id, label)| (*id, label.as_str())).collect();
    html! {
        section class="page settings settings-page" {
            header class="settings-header" {
                h1 { "Account settings" }
                p class="lede" { "Manage your profile, how you sign in, and your account." }
            }
            @if let Some(error) = view.flash.error { p class="alert" role="alert" { (error) } }
            @if let Some(notice) = view.flash.notice { p class="alert" role="status" { (notice) } }
            div class="settings-layout" {
                nav class="settings-toc" aria-label="Settings sections" {
                    (toc_group("Account", &account))
                    (toc_group("Sign-in & security", &security))
                    @if cfg!(feature = "stripe") { (toc_group("Billing", &[("billing", "Billing")])) }
                    @if cfg!(feature = "api") { (toc_group("Developers", &[("api-keys", "API keys"), ("connected-apps", "Connected apps")])) }
                    (toc_group("Danger zone", &[("delete-account", "Delete account")]))
                }
                div class="settings-sections" {
                    (group("Account", html! {
                        section class="panel" id="profile" aria-labelledby="profile-heading" {
                            (panel_head("profile-heading", "Profile", "Your name and the email address you sign in with."))
                            form class="form" method="post" action="/settings" {
                                input type="hidden" name="action" value="profile";
                                label { "Name" input name="name" autocomplete="name" maxlength="100" value=(profile.name); }
                                label { "Email address" input type="email" name="email" autocomplete="email" maxlength="254" required value=(profile.email); }
                                @if let Some(provider) = profile.confirm_with() {
                                    (confirmation_help(provider))
                                    p class="hint" { "Changing your email signs out your other sessions." }
                                } @else {
                                    label { "Current password" input type="password" name="password" autocomplete="current-password" maxlength="256" aria-describedby="password-help"; }
                                    p class="hint" id="password-help" { "Required only when changing your email. You’ll use the new email to sign in, and your other sessions will be signed out." }
                                }
                                button class="btn btn-primary" type="submit" { "Save changes" }
                            }
                        }
                        (avatar_panel(view, user))
                        section class="panel appearance-panel" id="appearance" aria-labelledby="appearance-heading" {
                            (panel_head("appearance-heading", "Appearance", "Saved in this browser. System follows your device."))
                            (super::theme_choices())
                        }
                    }))
                    (group("Sign-in & security", html! {
                        (sign_in_panels(profile, view))
                        (passkeys_panel(view.passkeys, profile.confirm_with()))
                        (super::two_factor::panel(&view.two_factor))
                    }))
                    @if cfg!(feature = "stripe") {
                        (group("Billing", html! {
                            section class="panel" id="billing" aria-labelledby="billing-heading" {
                                @if is_admin {
                                    (panel_head("billing-heading", "Billing", "Admin access"))
                                    p class="muted" { "Your admin role includes all paid features. No payment is required." }
                                } @else {
                                    (panel_head("billing-heading", "Billing", "Your plan, payment status, and billing options."))
                                }
                                a class="btn settings-upgrade" href="/billing" { "Manage billing" }
                            }
                        }))
                    }
                    @if cfg!(feature = "api") {
                        (group("Developers", html! { (api_keys_panel(view)) (connected_apps_panel(view)) }))
                    }
                    (group("Danger zone", html! {
                        section class="panel settings-danger" id="delete-account" aria-labelledby="delete-heading" {
                            (panel_head("delete-heading", "Delete account", "Permanently delete your account and local account data. This cannot be undone."))
                            @if cfg!(feature = "stripe") {
                                p class="muted" { "Resolve pending payments and cancel subscriptions in Billing first. Scheduled cancellations must finish before deletion. Deleting an account does not issue refunds; Stripe retains payment records." }
                            }
                            details open[view.flash.confirmation.is_some()] {
                                summary { "Delete my account…" }
                                form class="form" method="post" action="/settings" {
                                    input type="hidden" name="action" value="delete";
                                    @if let Some(provider) = profile.confirm_with() { (confirmation_help(provider)) } @else {
                                        label { "Current password" input type="password" name="password" autocomplete="current-password" maxlength="256" required; }
                                    }
                                    label { "Type DELETE to confirm" input name="confirmation" autocomplete="off" pattern="DELETE" required value=(view.flash.confirmation.unwrap_or_default()); }
                                    button class="btn btn-danger" type="submit" { "Permanently delete account" }
                                }
                            }
                        }
                    }))
                }
            }
        }
    }
}

/// One labelled cluster of panels; the table of contents mirrors these.
fn group(title: &str, panels: Markup) -> Markup {
    html! {
        div class="settings-group" {
            h2 class="settings-group-title" { (title) }
            (panels)
        }
    }
}

fn toc_group(title: &str, links: &[(&str, &str)]) -> Markup {
    html! {
        div class="settings-toc-group" {
            p class="settings-toc-title" { (title) }
            ul { @for (id, label) in links { li { a href={ "#" (id) } { (label) } } } }
        }
    }
}

/// A panel's title and a one-line summary, so the page scans top to bottom.
pub fn panel_head(id: &str, title: &str, summary: &str) -> Markup {
    html! {
        div class="panel-head" {
            h3 id=(id) { (title) }
            p class="muted small" { (summary) }
        }
    }
}

/// Table-of-contents entries for the sign-in provider panels: configured, or
/// already linked to this account.
fn sign_in_toc(view: &View<'_>) -> Vec<(&'static str, String)> {
    #[cfg(feature = "social-login")]
    return view.sign_in_methods.iter().map(|m| (m.id, format!("{} sign-in", m.name))).collect();
    #[cfg(not(feature = "social-login"))]
    {
        let _ = view;
        Vec::new()
    }
}

/// The first letter of the email, the stand-in for accounts without a picture.
pub fn initial(user: &User) -> String {
    user.email.chars().next().unwrap_or('A').to_uppercase().collect()
}

#[cfg(not(feature = "storage"))]
fn avatar_panel(_: &View<'_>, _: &User) -> Markup {
    html! {}
}

/// Profile pictures are the example of file storage; see `avatars.rs`.
#[cfg(feature = "storage")]
fn avatar_panel(view: &View<'_>, user: &User) -> Markup {
    html! {
        section class="panel" id="avatar" aria-labelledby="avatar-heading" {
            (panel_head("avatar-heading", "Profile picture", "Shown next to your account menu."))
            div class="avatar-editor" {
                @match &view.avatar_url {
                    Some(url) => { img class="avatar-preview" src=(url) alt="Your current profile picture" width="96" height="96"; }
                    None => { div class="avatar-preview avatar-placeholder" aria-hidden="true" { (initial(user)) } }
                }
                div class="avatar-forms" {
                    form class="form" method="post" action="/settings/avatar" enctype="multipart/form-data" {
                        label { "New picture"
                            input type="file" name=(crate::avatars::FIELD) accept="image/png,image/jpeg,image/gif,image/webp" required;
                            span class="hint" { "PNG, JPEG, GIF or WebP, up to 2 MB." }
                        }
                        button class="btn btn-primary" type="submit" { "Upload picture" }
                    }
                    @if view.avatar_url.is_some() {
                        form method="post" action="/settings" {
                            input type="hidden" name="action" value="remove_avatar";
                            button class="btn" type="submit" { "Remove picture" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(not(feature = "api"))]
fn api_keys_panel(_: &View<'_>) -> Markup {
    html! {}
}

#[cfg(feature = "api")]
fn api_keys_panel(view: &View<'_>) -> Markup {
    let at_limit = view.keys.len() >= api_keys::MAX_PER_USER as usize;
    html! {
        section class="panel" id="api-keys" aria-labelledby="api-keys-heading" {
            (panel_head("api-keys-heading", "API keys", "For scripts, other apps and coding agents."))
            p class="muted" {
                "Interact with this platform from scripts and other apps through its API. A key has the same access as your account, so keep it secret. Revoking a key takes effect immediately."
                @if cfg!(feature = "docs") { " See the " a href="/docs/api" { "API guide" } "." }
                @if cfg!(all(feature = "docs", feature = "mcp")) { " The same keys connect coding agents to the " a href="/docs/mcp" { "MCP server" } "." }
            }
            @if let Some(token) = view.flash.new_key {
                div class="api-key-reveal" role="status" {
                    p { strong { "Copy your new API key now. It will not be shown again." } }
                    pre { code { (token) } }
                    p class="muted small" { "Send it as " code { "Authorization: Bearer " (token.get(..api_keys::PREFIX.len() + 4).unwrap_or_default()) "…" } " on every request." }
                }
            }
            @if view.keys.is_empty() {
                p class="muted small" { "You have no API keys." }
            } @else {
                div class="admin-table-wrap" {
                    table class="admin-table" {
                        thead { tr { th { "Name" } th { "Key" } th { "Created" } th { "Last used" } th { span class="visually-hidden" { "Actions" } } } }
                        tbody {
                            @for key in view.keys {
                                tr {
                                    th scope="row" { (key.name) }
                                    td { code { (key.prefix) "…" } }
                                    td { (super::short_date(key.created_at)) }
                                    td { @match key.last_used_at { Some(at) => (super::short_date(at)), None => "Never" } }
                                    td {
                                        form method="post" action="/settings" {
                                            input type="hidden" name="action" value="revoke_api_key";
                                            input type="hidden" name="id" value=(key.id);
                                            button class="btn btn-danger" type="submit" { "Revoke" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            @if at_limit {
                p class="muted small" { "You have reached the limit of " (api_keys::MAX_PER_USER) " keys. Revoke one to create another." }
            } @else {
                form class="form" method="post" action="/settings" {
                    input type="hidden" name="action" value="create_api_key";
                    label { "Key name"
                        input name="name" maxlength=(api_keys::NAME_MAX) autocomplete="off" placeholder="e.g. laptop, CI, Claude Code";
                        span class="hint" { "Optional. Helps you recognize which key to revoke later." }
                    }
                    button class="btn btn-primary" type="submit" { "Create API key" }
                }
            }
        }
    }
}

#[cfg(not(feature = "api"))]
fn connected_apps_panel(_: &View<'_>) -> Markup {
    html! {}
}

/// Applications that connected through OAuth (`crate::oauth_server`); each row is one grant.
#[cfg(feature = "api")]
fn connected_apps_panel(view: &View<'_>) -> Markup {
    html! {
        section class="panel" id="connected-apps" aria-labelledby="connected-apps-heading" {
            (panel_head("connected-apps-heading", "Connected apps", "Agents and applications you signed in to with this account."))
            p class="muted" {
                "Applications that support OAuth, such as hosted AI assistants, connect with a sign-in and your approval instead of a key. Each has the same access as your account. Disconnecting takes effect immediately."
                @if cfg!(all(feature = "docs", feature = "mcp")) { " See the " a href="/docs/mcp" { "MCP server guide" } "." }
            }
            @if view.grants.is_empty() {
                p class="muted small" { "No applications are connected." }
            } @else {
                div class="admin-table-wrap" {
                    table class="admin-table" {
                        thead { tr { th { "Application" } th { "Connected" } th { "Last used" } th { "Lapses" } th { span class="visually-hidden" { "Actions" } } } }
                        tbody {
                            @for grant in view.grants {
                                tr {
                                    th scope="row" {
                                        (grant.client_name)
                                        @if let Some(uri) = &grant.client_uri { " " a class="muted small" href=(uri) rel="noopener noreferrer" { "↗" } }
                                    }
                                    td { (super::short_date(grant.created_at)) }
                                    td { @match grant.last_used_at { Some(at) => (super::short_date(at)), None => "Never" } }
                                    td { (super::short_date(grant.expires_at)) }
                                    td {
                                        form method="post" action="/settings" {
                                            input type="hidden" name="action" value="disconnect_app";
                                            input type="hidden" name="id" value=(grant.id);
                                            button class="btn btn-danger" type="submit" { "Disconnect" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Passkeys are added by `passkeys.js`; without it the form explains why.
/// Passwordless accounts confirm with their sign-in provider instead of a password they do not have.
pub fn passkeys_panel(passkeys: &[Passkey], confirm_with: Option<&str>) -> Markup {
    let day = format_description!("[day] [month repr:short] [year]");
    html! {
        section class="panel" id="passkeys" aria-labelledby="passkeys-heading" {
            (panel_head("passkeys-heading", "Passkeys", "Sign in with Face ID, Touch ID, Windows Hello, or a security key instead of your password. A passkey only works on this site, so it cannot be phished."))
            @if passkeys.is_empty() {
                p class="muted small" { "No passkeys yet." }
            } @else {
                ul class="passkey-list" {
                    @for passkey in passkeys {
                        li {
                            div {
                                strong { (passkey.name) }
                                @if passkey.backed_up { " " span class="badge" { "Synced" } }
                                p class="muted small" {
                                    "Added " (passkey.created_at.format(day).unwrap_or_default())
                                    @match passkey.last_used_at {
                                        Some(used) => { " · Last used " (used.format(day).unwrap_or_default()) }
                                        None => { " · Never used" }
                                    }
                                }
                            }
                            form method="post" action="/settings" {
                                input type="hidden" name="action" value="remove_passkey";
                                input type="hidden" name="id" value=(passkey.id);
                                button class="btn btn-danger" type="submit" { "Remove" }
                            }
                        }
                    }
                }
            }
            form class="form" method="post" action="/settings" data-passkey-register data-options-url="/passkeys/register/options" data-register-url="/passkeys/register" {
                input type="hidden" name="action" value="passkey";
                label { "Passkey name" input name="name" maxlength="64" autocomplete="off" placeholder="e.g. Laptop"; }
                @if let Some(provider) = confirm_with { (confirmation_help(provider)) } @else {
                    label { "Current password" input type="password" name="password" autocomplete="current-password" maxlength="256" required aria-describedby="passkey-help"; }
                }
                p class="hint" id="passkey-help" { "Confirm your identity, then follow your browser’s prompt to create the passkey." }
                button class="btn btn-primary" type="submit" { "Add a passkey" }
                p class="hint" data-passkey-status role="status" aria-live="polite" {}
            }
        }
    }
}

/// Link out of sensitive forms rather than nesting another form inside them.
/// Only passwordless accounts (`google` or `github` feature) ever see this.
pub fn confirmation_help(provider: &str) -> Markup {
    #[cfg(feature = "social-login")]
    let name = crate::social_login::name(provider);
    #[cfg(not(feature = "social-login"))]
    let name = provider;
    html! { p class="hint" { "First " a href={ "/settings#" (provider) } { "confirm with " (name) } ", then return here. Confirmation lasts five minutes in this browser." } }
}

#[cfg(not(feature = "social-login"))]
fn sign_in_panels(_: &Profile, _: &View<'_>) -> Markup {
    html! {}
}

/// One panel per provider shown in settings; see `social_login.rs`.
#[cfg(feature = "social-login")]
fn sign_in_panels(profile: &Profile, view: &View<'_>) -> Markup {
    let linked = view.sign_in_methods.iter().filter(|m| m.linked).count();
    html! { @for method in &view.sign_in_methods { (sign_in_panel(profile, method, linked)) } }
}

/// Connect, confirm with, or disconnect one provider. `linked` counts the
/// account's connected providers: a passwordless account always keeps one.
#[cfg(feature = "social-login")]
fn sign_in_panel(profile: &Profile, method: &crate::social_login::Method, linked: usize) -> Markup {
    let (id, name) = (method.id, method.name);
    let heading = format!("{id}-heading");
    let action = format!("/settings/{id}");
    html! {
        section class="panel" id=(id) aria-labelledby=(heading) {
            (panel_head(&heading, &format!("{name} sign-in"), &format!("Use your {name} account to sign in.")))
            @if method.linked {
                p { (name) " is connected to your account." }
                @if profile.passwordless {
                    @if linked == 1 {
                        p class="muted" { "Your account uses " (name) " and has no password. Confirm with " (name) " before changing your email, adding a passkey, managing two-factor authentication, or deleting your account." }
                    }
                    @if method.available {
                        form class="form" method="post" action=(action) {
                            input type="hidden" name="action" value="confirm";
                            button class="btn" type="submit" { "Confirm with " (name) }
                        }
                    } @else { p class="alert" { (name) " confirmation is temporarily unavailable. Please try again later." } }
                    @if linked > 1 {
                        form class="form" method="post" action=(action) {
                            input type="hidden" name="action" value="disconnect";
                            @if let Some(provider) = profile.confirm_with() { (confirmation_help(provider)) }
                            button class="btn" type="submit" { "Disconnect " (name) }
                        }
                    }
                } @else {
                    form class="form" method="post" action=(action) {
                        input type="hidden" name="action" value="disconnect";
                        label { "Current password" input type="password" name="password" autocomplete="current-password" maxlength="256" required; }
                        button class="btn" type="submit" { "Disconnect " (name) }
                    }
                }
            } @else {
                p class="muted" { "Connect " (name) " to sign in to this same account with one click." }
                form class="form" method="post" action=(action) {
                    input type="hidden" name="action" value="link";
                    @if let Some(provider) = profile.confirm_with() { (confirmation_help(provider)) } @else {
                        label { "Current password" input type="password" name="password" autocomplete="current-password" maxlength="256" required; }
                    }
                    button class="btn" type="submit" { "Connect " (name) }
                }
            }
        }
    }
}
