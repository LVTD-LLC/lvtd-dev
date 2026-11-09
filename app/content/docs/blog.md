+++
title = "Blog"
slug = "blog"
description = "Publish Markdown articles with an RSS feed built into your app."
section = "features"
order = 1
+++

## Enable Blog

The `blog` Cargo feature adds article pages at `/blog/{slug}`, a listing at `/blog`, an RSS feed at `/blog/rss.xml`, and links in the navigation and sitemap.

Your project's default features are in `app/Cargo.toml`. Add `"blog"` to enable the feature, or remove it to omit those routes and embedded posts from the build. The source remains available either way.

## Write a post

Replace the sample in `app/content/blog/`, or add a Markdown file:

```toml
+++
title = "Our first release"
slug = "our-first-release"
description = "What we shipped and what comes next."
author = "Your team"
date = "2026-10-07"
draft = true
+++
```

Write the article below the closing delimiter. The title becomes the page heading; start body headings at `##`. Use a unique slug with lowercase letters, digits, and single hyphens. Raw HTML is displayed as text.

## Publish

Set `draft = false`, or remove the draft field. Drafts are excluded from article pages, the listing, RSS, and the sitemap. A future date does not schedule publication.

Run the checks in your generated `AGENTS.md`, preview locally, then rebuild and deploy. Markdown is embedded in your app and rendered at startup, so editing a file on a running server does not publish it.
