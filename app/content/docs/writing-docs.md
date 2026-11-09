+++
title = "Writing documentation"
slug = "writing-docs"
description = "Add Markdown guides and publish your product documentation."
section = "features"
order = 2
+++

## Enable documentation

The `docs` Cargo feature adds this documentation section. Add or remove `"docs"` in the default features in `app/Cargo.toml` to enable or disable its routes, navigation, content, and sitemap entries. Source files stay available when the feature is disabled.

These guides describe the starter code. Replace them with documentation for your product as it grows.

## Write a guide

Add a Markdown file to `app/content/docs/` with TOML front matter:

```toml
+++
title = "Your guide"
slug = "your-guide"
description = "What readers will learn."
section = "features"
order = 3
+++
```

Write the guide below the closing delimiter. The title becomes the page heading, the slug sets its `/docs/your-guide` address, and `order` controls its position within a navigation section. Set `draft = true` to keep a guide unpublished.

## Organize navigation

Set `section` to `getting-started`, `features`, `deployment`, or `developer-tools`. Sections appear in that order and are hidden when empty. Use Features for app capabilities and integrations; use Developer tools for API, MCP, or CLI guides when your product supports them. Within each section, ties in `order` are sorted by slug.

The required published `welcome` page supplies the content at `/docs`. Its old `/docs/welcome` URL redirects to `/docs`. Keep that slug when replacing the introduction.

Guides for optional features remain available even when those features are disabled, so you can follow their setup instructions later. Remove or draft any starter guides you do not want to publish.

## Docs for AI assistants

`/llms.txt` serves a Markdown navigation index with your app's name, the welcome page's description, and links to every published guide. It also links to `/llms-full.txt`, which contains the guides in full, grouped by section. Both files are built at startup from the same content as the HTML pages, so edits appear automatically and drafts stay out. Both routes require the `docs` feature. The index must stay under 30,000 characters; startup validation rejects larger indexes. Keep descriptions concise, and move additional navigation into linked section indexes if needed.

## Publish changes

Rebuild and deploy the app to publish your edits. Guides are validated and rendered at startup, so requests do not parse Markdown.
