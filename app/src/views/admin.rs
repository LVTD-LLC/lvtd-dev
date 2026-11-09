use crate::admin::Account;
use maud::{Markup, html};
use uuid::Uuid;

pub fn page(accounts: &[Account], next: Option<Uuid>, paginated: bool) -> Markup {
    html! {
        section class="page admin-page" {
            div class="page-head" {
                div {
                    h1 { "Admin" }
                    p class="lede" { "An overview of " (super::SITE_NAME) " accounts." }
                }
                a class="btn" href="/dashboard" { "Back to dashboard" }
            }
            h2 { "Users" }
            p class="muted admin-description" { "Showing up to 50 accounts per page, newest first." }
            @if accounts.is_empty() {
                div class="empty" { p { "No users on this page." } }
            } @else {
                div class="admin-table-wrap" role="region" aria-label="User accounts" tabindex="0" {
                    table class="admin-table" {
                        caption { "User accounts and access" }
                        thead { tr {
                            th scope="col" { "Email" }
                            th scope="col" { "Joined" }
                            th scope="col" { "Role" }
                        } }
                        tbody {
                            @for account in accounts {
                                tr {
                                    th scope="row" { (account.email) }
                                    td { (account.created_at.date()) }
                                    td { (if account.is_admin { "Admin" } else { "User" }) }
                                }
                            }
                        }
                    }
                }
            }
            nav class="admin-pagination" aria-label="User pagination" {
                @if paginated { a class="btn" href="/admin" { "Newest users" } }
                @if let Some(before) = next { a class="btn" href={ "/admin?before=" (before) } { "Older users" } }
            }
        }
    }
}
