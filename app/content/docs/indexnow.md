+++
title = "Automatic IndexNow"
slug = "indexnow"
description = "Notify participating search engines about published public page changes automatically."
section = "features"
order = 60
+++


## Setup

IndexNow is included automatically; no account, API key or scheduled workflow
is needed. Set `SITE_URL` to your actual public HTTPS origin (for example,
`https://your-product.com`). Normal startup creates a random per-origin key in
Postgres and serves `/<key>.txt` as plain text. Local HTTP, IP addresses and
reserved development domains do not submit. Set `INDEXNOW_ENABLED=false` on
public staging/preview deployments or to opt out. Keep `SITE_URL` canonical.

## What gets submitted

After startup, a background worker checks that the public key endpoint is
serving this build, then notifies IndexNow about the homepage and enabled,
published blog/docs pages. Drafts and account/auth/billing URLs are excluded.
App source, assets, content or feature changes notify the current public page
set; removed pages are also submitted from the previous accepted snapshot.
This deliberately sends all public pages when the build fingerprint changes,
not just pages with a changed publication date. Unchanged restarts send nothing.

## Reliability

The worker retries errors with backoff (up to one hour), checks every 15 minutes
after success, and only advances its durable snapshot after every batch is
accepted. Replica row locks prevent simultaneous submissions. A crash after
acceptance but before saving can repeat a batch safely. Keep the Postgres
database across deploys; deleting its state loses removal history.

Public ownership endpoints must be accessible without authentication, redirects
or proxy caching, including the `x-indexnow-revision` header. The fingerprint
works without Git or a particular hosting provider. Rolling deployments may
delay notifications until the public endpoint matches the worker's build.

## Extending public content

For new public route types, extend `indexnow_paths` and the sitemap together.
Content here is embedded at build time: publish edits by rebuilding/deploying.
Future database-backed content needs a publish/update/delete integration; the
startup snapshot does not watch arbitrary database rows.

## Verify submissions

Successful submission logs `IndexNow URLs accepted`; HTTP 200 means received
and 202 means received with ownership validation pending. Neither guarantees
crawling, indexing or ranking. IndexNow complements the sitemap; it does not
replace Google Search Console. See the [IndexNow protocol](https://www.indexnow.org/documentation).
