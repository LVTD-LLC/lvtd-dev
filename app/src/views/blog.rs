use crate::blog::{Blog, Post};
use maud::{Markup, html};

pub fn index(blog: &Blog) -> Markup {
    html! {
        section class="page blog" {
            header class="blog-heading" {
                p class="eyebrow" { (super::SITE_NAME) " blog" }
                h1 { "News and updates" }
                p class="lede" { (crate::blog::DESCRIPTION) }
                a class="blog-feed" href="/blog/rss.xml" { "Subscribe via RSS" }
            }
            @if blog.posts.is_empty() {
                p class="muted" { "The first post is on its way." }
            }
            @for post in &blog.posts {
                article class="blog-entry" {
                    (byline(post))
                    h2 { a href=(post.path) { (post.title) } }
                    p class="muted" { (post.description) }
                    a class="blog-read" href=(post.path) aria-label={ "Read " (post.title) } { "Read article →" }
                }
            }
        }
    }
}

pub fn article(post: &Post) -> Markup {
    html! {
        article class="page blog" {
            a class="blog-back" href="/blog" { "← All posts" }
            header class="blog-heading" {
                (byline(post))
                h1 { (post.title) }
                p class="lede" { (post.description) }
            }
            div class="prose" { (&post.body) }
            footer class="blog-end" {
                a href="/blog" { "← All posts" }
                a href="/blog/rss.xml" { "Subscribe via RSS" }
            }
        }
    }
}

fn byline(post: &Post) -> Markup {
    html! {
        p class="blog-meta" {
            time datetime=(post.date) { (post.display_date) }
            " · " (post.author)
        }
    }
}
