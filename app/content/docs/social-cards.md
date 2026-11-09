+++
title = "Social cards"
slug = "social-cards"
description = "Control how links to your site look when shared on X, LinkedIn, Slack and other apps."
section = "features"
order = 8
+++

## What ships

Every public page has a title, description, canonical URL, Open Graph metadata
and a large X (Twitter) card. The homepage, legal pages, blog posts and docs each
get a generated 1200×630 PNG with your project name and page title. Homepage and
legal cards work even with the blog and docs features disabled.

Set `SITE_URL` to your public origin. Canonical links and image URLs use that
address, independent of incoming host headers or tracking parameters.
`/robots.txt` points crawlers to your sitemap. Public pages allow large image
previews in search; account pages and errors ask crawlers not to index them.

## Customize your cards

Update `SITE_NAME`, `HOME_TITLE` and `SITE_DESCRIPTION` in `app/src/views/mod.rs`
alongside your landing-page copy. The generator fills the name automatically.
Legal descriptions live in `views/legal.rs`, About and Contact in
`views/company.rs`; blog and docs descriptions come from Markdown front matter. Change the colors in `THEME` in `app/src/og.rs`.

Cards render once at startup using the bundled Inter font. Titles shrink and
wrap to fit three lines. `/og/…` serves the PNGs; the homepage uses `/og/index.png`.
The image URL includes a content hash, so changes produce a new preview URL.

## Add a public page

Give the page a unique title and pass its summary to `views::layout_with_metadata`.
Register it with `routes::public_page`, add its metadata to
`structured_data::build`, its card to `og::load`, and its URL to both the sitemap
and `indexnow_paths`. The public metadata registration also enables indexing.
The regression test follows every sitemap URL and checks the metadata and image.

To use custom artwork for a page, return your embedded image and alt text from
`views::og_image` before the generated-card lookup. Use a 1200×630 PNG. The
existing `app/og-image/og.html` and `render.sh` provide an editable starting point;
`app/assets/og-image.png` remains the fallback for pages without a generated card.

## Check a preview

After deploying, paste a URL into the post composer on X or LinkedIn, or into
a Slack message, to see the card. Each service caches previews for a while,
so a changed title or description can take time to appear.
