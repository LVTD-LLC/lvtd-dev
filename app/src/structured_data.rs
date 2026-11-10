//! Public page identity, serialized once at startup. Never include account data.

use crate::{AppState, views};
use maud::{Markup, PreEscaped, html};
use serde_json::{Value, json};
use std::collections::HashMap;

/// JSON in a script is raw text: HTML entities would corrupt it, while an
/// unescaped `<` could close the script element even inside a JSON string.
fn script(value: Value) -> Markup {
    html! { script type="application/ld+json" { (PreEscaped(value.to_string().replace('<', "\\u003c"))) } }
}

pub fn build(state: &AppState) -> HashMap<String, Markup> {
    let origin = state.cfg.site_url.trim_end_matches('/');
    let identity = identity(origin);
    let website = json!({
        "@type": "WebSite", "@id": format!("{origin}/#website"),
        "name": views::SITE_NAME, "description": views::SITE_DESCRIPTION,
        "url": format!("{origin}/"), "about": {"@id": format!("{origin}/#product")}
    });
    let mut pages = HashMap::new();
    let mut add = |path: &str, kind: &str, title: &str, description: &str, extra: Value| {
        let url = format!("{origin}{path}");
        let mut page = json!({
            "@type": kind, "@id": format!("{url}#page"),
            "name": title, "description": description, "url": url,
            "isPartOf": {"@id": format!("{origin}/#website")},
            "about": {"@id": format!("{origin}/#product")}, "inLanguage": "en"
        });
        page.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        pages.insert(
            path.to_string(),
            script(json!({
                "@context": "https://schema.org", "@graph": [identity, website, page]
            })),
        );
    };
    add(
        "/",
        "WebPage",
        views::SITE_NAME,
        views::SITE_DESCRIPTION,
        json!({"mainEntity": {"@id": format!("{origin}/#product")}}),
    );
    for &(path, title, description) in views::legal::PAGES {
        add(path, "WebPage", title, description, json!({}));
    }
    for page in crate::studio::PAGES {
        add(page.path, "WebPage", page.title, page.description, json!({}));
    }
    let organization = json!({"@type": "Organization", "name": views::legal::COMPANY,
        "email": views::legal::CONTACT_EMAIL, "founder": {"@type": "Person", "name": views::company::FOUNDER}});
    for &(path, title, description) in views::company::PAGES {
        let kind = if path == "/about" { "AboutPage" } else { "ContactPage" };
        add(path, kind, title, description, json!({"mainEntity": organization}));
    }
    #[cfg(feature = "blog")]
    {
        add("/blog", "Blog", "Blog", crate::blog::DESCRIPTION, json!({}));
        for post in &state.blog.posts {
            add(
                &post.path,
                "BlogPosting",
                &post.title,
                &post.description,
                json!({
                    "headline": post.title, "datePublished": post.date.to_string(),
                    "author": {"name": post.author},
                    "mainEntityOfPage": format!("{origin}{}", post.path),
                    "image": format!("{origin}/og{}.png", post.path)
                }),
            );
        }
    }
    #[cfg(feature = "docs")]
    for doc in std::iter::once(&state.docs.welcome).chain(&state.docs.pages) {
        add(&doc.path, "TechArticle", &doc.title, &doc.description, json!({"headline": doc.title}));
    }
    pages
}

/// Customize this identity to match your product and visible landing-page copy.
/// Add only real offers and official sameAs profiles; never invent prices or ratings.
fn identity(origin: &str) -> Value {
    json!({
        "@type": "SoftwareApplication", "@id": format!("{origin}/#product"),
        "name": views::SITE_NAME, "description": views::SITE_DESCRIPTION,
        "url": format!("{origin}/"), "applicationCategory": "BusinessApplication",
        "operatingSystem": "Web browser"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_round_trips_without_html_entities_or_script_breakouts() {
        let value = json!({"name": "Quotes \" & </ScRiPt><script>alert(1)</script>\n雪"});
        let html = script(value.clone()).into_string();
        let raw =
            html.strip_prefix("<script type=\"application/ld+json\">").unwrap().strip_suffix("</script>").unwrap();
        assert!(!raw.contains('<'));
        assert!(!raw.contains("&quot;"));
        assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), value);
    }
}
