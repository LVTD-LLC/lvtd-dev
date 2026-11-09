//! Per-page social cards: a generated 1200×630 link-preview image for every
//! marketing, legal, blog and docs page, drawn with `app_core::og` once per
//! process, including builds without blog or docs. See /docs/social-cards.

use crate::{SharedState, views::SITE_NAME};
use anyhow::Result;
use app_core::og::{Card, Theme};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;

/// Card colors as `0xRRGGBB`; match them to your brand.
pub const THEME: Theme = Theme::DARK;

/// One page's card, keyed by the page path.
struct Image {
    png: Bytes,
    /// Short content hash: the URL changes whenever the image does.
    version: String,
    alt: String,
}

pub struct Cards {
    by_path: HashMap<String, Image>,
}

impl Cards {
    /// The card for a page path: its URL (relative to `SITE_URL`) and alt text.
    pub fn image(&self, path: &str) -> Option<(String, &str)> {
        self.by_path.get(path).map(|image| {
            (format!("/og{}.png?v={}", if path == "/" { "/index" } else { path }, image.version), image.alt.as_str())
        })
    }
}

/// What one page's card says.
struct Page {
    path: String,
    label: String,
    title: String,
    description: String,
}

/// Cards depend only on embedded content, so one render serves every
/// `AppState` in the process (tests build many).
pub fn load(
    #[cfg(feature = "blog")] blog: &crate::blog::Blog,
    #[cfg(feature = "docs")] docs: &crate::docs::Docs,
) -> &'static Cards {
    static CARDS: OnceLock<Cards> = OnceLock::new();
    CARDS.get_or_init(|| {
        let mut pages = vec![Page {
            path: "/".into(),
            label: "".into(),
            title: crate::views::HOME_TITLE.into(),
            description: crate::views::SITE_DESCRIPTION.into(),
        }];
        pages.extend(crate::views::company::PAGES.iter().map(|&(path, title, description)| Page {
            path: path.into(),
            label: "Company".into(),
            title: title.into(),
            description: description.into(),
        }));
        pages.extend(crate::views::legal::PAGES.iter().map(|&(path, title, description)| Page {
            path: path.into(),
            label: "Legal".into(),
            title: title.into(),
            description: description.into(),
        }));
        #[cfg(feature = "blog")]
        {
            pages.push(Page {
                path: "/blog".into(),
                label: "Blog".into(),
                title: "Latest articles".into(),
                description: crate::blog::DESCRIPTION.into(),
            });
            pages.extend(blog.posts.iter().map(|post| Page {
                path: post.path.clone(),
                label: "Blog".into(),
                title: post.title.clone(),
                description: post.description.clone(),
            }));
        }
        #[cfg(feature = "docs")]
        for (doc, label) in std::iter::once((&docs.welcome, "Docs".to_owned()))
            .chain(docs.pages.iter().map(|doc| (doc, format!("Docs · {}", doc.section.title()))))
        {
            pages.push(Page {
                path: doc.path.clone(),
                label,
                title: doc.title.clone(),
                description: doc.description.clone(),
            });
        }
        render_all(pages).expect("social cards must render before serving")
    })
}

/// Rendering is CPU-bound and independent per page, so spread it over the cores.
fn render_all(pages: Vec<Page>) -> Result<Cards> {
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = pages.len().div_ceil(threads).max(1);
    let rendered = std::thread::scope(|scope| {
        let workers: Vec<_> = pages
            .chunks(chunk)
            .map(|pages| scope.spawn(|| pages.iter().map(render).collect::<Result<Vec<_>>>()))
            .collect();
        workers.into_iter().map(|worker| worker.join().expect("card rendering panicked")).collect::<Result<Vec<_>>>()
    })?;
    Ok(Cards { by_path: rendered.into_iter().flatten().collect() })
}

fn render(page: &Page) -> Result<(String, Image)> {
    let card =
        Card { site_name: SITE_NAME, label: &page.label, title: &page.title, description: Some(&page.description) };
    let png = app_core::og::render(&card, &THEME)?;
    let version = Sha256::digest(&png)[..6].iter().map(|b| format!("{b:02x}")).collect();
    let image = Image { png: png.into(), version, alt: page.title.clone() };
    Ok((page.path.clone(), image))
}

/// `GET /og/{page path}.png`, e.g. `/og/blog/welcome.png`.
pub async fn serve(State(state): State<SharedState>, Path(file): Path<String>, headers: HeaderMap) -> Response {
    let image = file.strip_suffix(".png").and_then(|path| {
        let path = if path == "index" { "/".to_owned() } else { format!("/{path}") };
        state.og.by_path.get(&path)
    });
    let Some(image) = image else {
        return crate::errors::not_found(&headers, || StatusCode::NOT_FOUND.into_response());
    };
    (
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
        image.png.clone(),
    )
        .into_response()
}
