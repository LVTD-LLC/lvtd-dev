//! Repository Markdown, validated and rendered once at startup.

use anyhow::{Context, Result, ensure};
use axum::body::Bytes;
use maud::{Markup, PreEscaped, html};
use pulldown_cmark::{Event, Options, Parser};
use rust_embed::Embed;
use serde::Deserialize;
use std::collections::HashSet;
use time::{Date, format_description::well_known::Rfc2822, macros::format_description};

pub const DESCRIPTION: &str = "Notes from shipping: product engineering, automation, and research at LVTD.";

#[derive(Embed)]
#[folder = "content/blog/"]
#[include = "*.md"]
struct Content;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    title: String,
    slug: String,
    description: String,
    author: String,
    date: String,
    #[serde(default)]
    draft: bool,
}

pub struct Post {
    pub title: String,
    pub slug: String,
    pub description: String,
    pub author: String,
    pub date: Date,
    pub display_date: String,
    pub path: String,
    pub body: Markup,
}

pub struct Blog {
    /// Published posts, newest first. Drafts never enter public state.
    pub posts: Vec<Post>,
    pub rss: Bytes,
}

impl Blog {
    pub fn load(site_url: &str) -> Result<Self> {
        let sources = Content::iter()
            .map(|name| {
                let file = Content::get(&name).context("embedded blog file missing")?;
                Ok((name.into_owned(), String::from_utf8(file.data.into_owned())?))
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_sources(sources, site_url)
    }

    fn from_sources(sources: Vec<(String, String)>, site_url: &str) -> Result<Self> {
        let mut posts = Vec::new();
        let mut slugs = HashSet::new();
        for (name, source) in sources {
            let (meta, post) = parse(&source).with_context(|| format!("invalid blog post {name}"))?;
            ensure!(slugs.insert(meta.slug), "duplicate blog slug in {name}");
            if !meta.draft {
                posts.push(post);
            }
        }
        posts.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.slug.cmp(&b.slug)));
        let rss = feed(&posts, site_url).into();
        Ok(Self { posts, rss })
    }
}

fn parse(source: &str) -> Result<(FrontMatter, Post)> {
    let source = source.replace("\r\n", "\n");
    let source = source.strip_prefix("+++\n").context("expected +++ TOML front matter")?;
    let (front_matter, markdown) = source.split_once("\n+++\n").context("missing closing +++ delimiter")?;
    let meta: FrontMatter = toml::from_str(front_matter)?;
    ensure!(!meta.title.trim().is_empty(), "title is empty");
    ensure!(!meta.description.trim().is_empty(), "description is empty");
    ensure!(!meta.author.trim().is_empty(), "author is empty");
    ensure!(!markdown.trim().is_empty(), "body is empty");
    ensure!(
        !meta.slug.is_empty()
            && meta
                .slug
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
        "slug must contain lowercase letters or digits separated by single hyphens"
    );
    let date = Date::parse(&meta.date, format_description!("[year]-[month]-[day]"))?;
    // RSS 2.0 dates use RFC 2822, which requires a year of at least 1900.
    ensure!(date.year() >= 1900, "date must be in 1900 or later");
    let mut body = String::new();
    let events = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH).map(|event| match event {
        // Articles are trusted repo content, but raw HTML isn't needed for writing.
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        other => other,
    });
    pulldown_cmark::html::push_html(&mut body, events);
    let post = Post {
        title: meta.title.clone(),
        slug: meta.slug.clone(),
        description: meta.description.clone(),
        author: meta.author.clone(),
        date,
        display_date: date.format(format_description!("[month repr:long] [day padding:none], [year]"))?,
        path: format!("/blog/{}", meta.slug),
        body: PreEscaped(body),
    };
    Ok((meta, post))
}

fn feed(posts: &[Post], site_url: &str) -> String {
    let origin = site_url.trim_end_matches('/');
    let xml = html! {
        rss version="2.0" {
            channel {
                title { (crate::views::SITE_NAME) " Blog" }
                link { (origin) "/blog" }
                description { (DESCRIPTION) }
                language { "en" }
                @for post in posts {
                    item {
                        title { (post.title) }
                        link { (origin) (post.path) }
                        guid isPermaLink="true" { (origin) (post.path) }
                        description { (post.description) }
                        pubDate { (post.date.midnight().assume_utc().format(&Rfc2822).expect("validated RSS date")) }
                    }
                }
            }
        }
    };
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}", xml.into_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(slug: &str, date: &str, draft: bool) -> (String, String) {
        (
            format!("{slug}.md"),
            format!(
                "+++\ntitle = \"Rust & HTML\"\nslug = \"{slug}\"\ndescription = \"A <guide> & notes\"\nauthor = \"Example author\"\ndate = \"{date}\"\ndraft = {draft}\n+++\n\n## Start here\n\nWrite **Rust**.\n\n```rust\nlet x = 1 < 2;\n```\n\n<script>alert(1)</script>\n"
            ),
        )
    }

    #[test]
    fn embedded_posts_are_valid() {
        Blog::load("https://example.com").unwrap();
    }

    #[test]
    fn publication_filters_drafts_sorts_and_renders_markdown() {
        let blog = Blog::from_sources(
            vec![
                source("old", "2026-09-01", false),
                source("secret", "2026-10-07", true),
                source("new", "2026-10-06", false),
            ],
            "https://example.com/",
        )
        .unwrap();
        assert_eq!(blog.posts.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(), ["new", "old"]);
        let body = blog.posts[0].body.clone().into_string();
        assert!(body.contains("<h2>Start here</h2>"));
        assert!(body.contains("<strong>Rust</strong>"));
        assert!(body.contains("language-rust"));
        assert!(body.contains("1 &lt; 2"));
        assert!(!body.contains("<script>"));
        let rss = std::str::from_utf8(&blog.rss).unwrap();
        assert!(!rss.contains("secret"));
        assert!(rss.contains("Rust &amp; HTML"));
        assert!(rss.contains("A &lt;guide&gt; &amp; notes"));
        assert!(rss.contains("<link>https://example.com/blog/new</link>"));
        assert!(rss.contains("Tue, 06 Oct 2026 00:00:00 +0000"));
    }

    #[test]
    fn invalid_content_and_duplicate_slugs_fail_loudly() {
        for slug in ["../escape", "UPPER", "two--dashes", "-leading", "trailing-", "rss.xml", ""] {
            assert!(Blog::from_sources(vec![source(slug, "2026-10-06", false)], "https://example.com").is_err());
        }
        for date in ["2026-02-30", "not-a-date", "1899-01-01"] {
            assert!(Blog::from_sources(vec![source("post", date, false)], "https://example.com").is_err());
        }
        let duplicate = vec![source("same", "2026-10-06", false), source("same", "2026-10-05", true)];
        assert!(Blog::from_sources(duplicate, "https://example.com").is_err());
        assert!(parse("No front matter").is_err());
        let (_, valid) = source("post", "2026-10-06", false);
        assert!(parse(&valid.replace("title = \"Rust & HTML\"", "title = \"\"")).is_err());
        assert!(parse(&valid.replace("description =", "typo =")).is_err());
        assert!(parse(&valid.replace('\n', "\r\n")).is_ok());
    }
}
