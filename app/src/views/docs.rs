use crate::docs::{Docs, Document};
use maud::{Markup, html};

pub fn page(docs: &Docs, current: &Document) -> Markup {
    html! {
        section class="page docs-layout" {
            nav class="docs-nav" aria-label="Documentation" {
                a class="docs-home" href="/docs" aria-current=[(current.path == "/docs").then_some("page")] { "Overview" }
                @for (index, doc) in docs.pages.iter().enumerate() {
                    @if index == 0 || docs.pages[index - 1].section != doc.section {
                        h2 class="docs-nav-heading" { (doc.section.title()) }
                    }
                    a href=(doc.path) aria-current=[(current.slug == doc.slug).then_some("page")] { (doc.title) }
                }
            }
            div class="docs-content" {
                header class="blog-heading" {
                    p class="eyebrow" { "Documentation" }
                    h1 { (current.title) }
                    p class="lede" { (current.description) }
                }
                article class="prose" { (current.body) }
            }
        }
    }
}
