# LVTD preview website

Public repository: https://github.com/LVTD-LLC/lvtd-dev

Preview: https://lvtd-dev.cr.lvtd.dev

Generated with every available ShipRust option, template `428d214`. The generated
screen layout, typography, themes, and CSS are retained. LVTD copy and public
content replace the sample copy; projects, research, AI Steering, Hosted OpenClaw,
legal pages, two blog articles, and all six public Jev question pages are included.
Existing trailing-slash blog URLs and `/tos/` redirect to their canonical pages.

`app/src/studio.rs` registers imported public pages. Their semantic HTML lives in
`app/content/site/`, without the old site's CSS or scripts. Blog articles use the
normal generated Markdown collection. The Jev pages, JSON evidence bundle, and
methodology are a **10 November 2026 public snapshot**, not a replacement for the
Django benchmark coordinator or its paid jobs. Refresh those files deliberately
when you want newer results. The public benchmark JSON retains its source export
timestamp and checksum.

The **Deploy preview** GitHub Action builds on GitHub-hosted Ubuntu runners,
publishes the image to GHCR, deploys `lvtd-dev` and `lvtd-dev-worker` to CapRover,
and verifies the exact commit via `/healthz`. Runtime secrets stay in CapRover;
GitHub stores only the instance URL and separate app deployment tokens. The
preview uses its own Postgres database and persistent upload volume. IndexNow is
disabled. No production domain is attached and the current `lvtd-site` app,
database, and workers are excluded from deployment.

All integrations are compiled in. Mailgun verification uses the existing LVTD
mail configuration and a new verification signing key. OAuth clients, Stripe
billing, Sentry, PostHog, and Qdrant remain inactive until configured for this
application; local persistent storage works without an S3 bucket. The preview
service page accepts inquiries by email, and does not initiate live checkout.
The generated account, API, MCP, docs, and billing surfaces remain available.

The sections below are ShipRust's generated development and integration guide.

---

# LVTD

A SaaS starter in Rust. Axum, Maud, Postgres, one static binary.

What's in this iteration:

- Landing page, sign up, sign in, sign out, signed-in dashboard
- Email + password accounts (argon2id), required email verification, server-side cookie sessions
- Passkeys (WebAuthn) for passwordless sign-in, attached by verified users in account settings
- Two-factor authentication with any authenticator app (TOTP) and single-use recovery codes
- Postgres via sqlx with compile-time checked queries and embedded migrations
- CSRF protection via same-origin checks + `SameSite=Lax` cookies
- Light/dark theme, hand-written CSS, content-hashed immutable assets
- End-to-end tests against a real database, CI, Dockerfile
- Dependency checks: CI fails on known vulnerabilities, and Dependabot opens weekly update pull requests

Accounts are the `auth` Cargo feature, on by default. See "Accounts (optional)".

## Run it

```bash
docker compose up -d        # Postgres 18 on localhost:5433
cp .env.example .env
SQLX_OFFLINE=true cargo run # http://localhost:3000
```

The first build checks SQL against the bundled `.sqlx` metadata because the
new database has no tables yet; startup applies the migrations. After that,
plain `cargo run` checks queries against the live database.

```bash
cargo test --workspace      # each test gets a fresh database
```

## Branch coverage

CI runs Rust branch coverage on every pull request and main push, using the
project's selected Cargo defaults. The job summary lists covered and uncovered
branch outcomes; download the `branch-coverage` artifact and open `html/index.html`
to inspect both sides of each instrumented decision. JSON and LCOV reports are
included for editor/tool integration. No external coverage service or token is needed.

To run it locally, start disposable Postgres, export `DATABASE_URL` (the database
user needs CREATEDB), and install the pinned tools:

```bash
rustup toolchain install nightly-2026-10-01 --profile minimal --component llvm-tools-preview,rustfmt
cargo install cargo-llvm-cov --version 0.9.1 --locked
bash scripts/coverage.sh
# Optional: measure code behind every feature, or a minimal build instead.
bash scripts/coverage.sh --all-features
bash scripts/coverage.sh --no-default-features
```

Reports are written to `target/coverage/` and replaced on each run. Normal builds
and tests stay on stable Rust; only instrumentation uses the dated nightly.
Update the nightly and cargo-llvm-cov pins in CI, the script and these instructions
together after checking real branch data is still produced.

Following Jason Turner's *Make Bugs (nearly) Impossible*, chapter 14, track
branch outcomes rather than treating executed lines as proof that both outcomes
were tested. This establishes a measured baseline, without an arbitrary percentage
gate: test failures and missing/empty branch instrumentation fail CI. Use uncovered
branches to choose meaningful tests; even 100% branch coverage does not establish
path coverage or correctness. LLVM reports Rust's instrumented branches; it does
not cover JavaScript, SQL, every macro expansion or every language construct.
Integration-test files and dependencies outside the workspace are excluded by
cargo-llvm-cov's defaults; inline unit-test code can appear in source-file totals.
See [cargo-llvm-cov's documentation](https://github.com/taiki-e/cargo-llvm-cov)
for the current nightly limitations.

## Email verification

Locally, signup works out of the box: with no mail provider configured the app
prints every email, verification code included, to its log. Sign up, copy the
six-character code from the terminal running `cargo run`, and enter it in the
browser. This console mode only starts on a non-HTTPS `SITE_URL`.

Before deploying, configure your own Mailgun US sending domain:
`MAILGUN_SENDING_KEY` (a domain-scoped sending key), `MAILGUN_DOMAIN`, and
`MAIL_FROM` (an address on that domain). Set an independent
`EMAIL_VERIFICATION_KEY` using `openssl rand -hex 32`. Keep all four values in
runtime secrets, never in source or template answers. Restart after changing them.

Signup emails a six-character code. Enter it in the same browser to create the
account and session. Codes expire in ten minutes, allow five guesses, and can be
resent after one minute. Each address can request five sends per hour, with a
200-per-hour global ceiling; these conservative limits live in
`app/src/verification.rs`. Resending replaces the previous code.

With the Mailgun settings blank on an HTTPS `SITE_URL`, the app refuses to
start rather than log codes in production. Partial configuration fails startup.
Email delivery failures never create accounts; the user can retry after the
resend cooldown. Verification is part of every build with accounts (the `auth`
feature), in console mode too. The Mailgun endpoint uses the US region.

Existing users are preserved during upgrades. This flow verifies new signups;
replacement email addresses in account settings are not yet reverified. The
verification module belongs to the app; shared auth crates remain independent
of the mail provider. Integration tests use a local mail server double and need
no credentials. See [the email verification guide](app/content/docs/email-verification.md).

## Accounts (optional)

The `auth` feature (on by default) is everything to do with accounts: sign-up
with email verification, sign-in and sign-out, passkeys, two-factor
authentication, account settings, the dashboard, the admin panel and its
`admin` command, the welcome email job and the hourly session cleanup. Remove
`"auth"` from the default features in `app/Cargo.toml` for a public site
without accounts: those routes are gone, the navbar has no sign-in buttons, the
homepage asks visitors to get in touch, and the Terms and Privacy pages drop
the account sections and Mailgun.

Google and GitHub sign-in, the API, MCP server, file storage, Stripe billing and Qdrant act
on behalf of a signed-in user, so each of those features turns `auth` on. The auth crate, the
migrations and their tables stay in every build, so adding accounts later is a
one-word change to the default features.

## Passkeys

Signed-in users add a passkey in account settings after confirming their
password; the sign-in page then offers passwordless sign-in, including the
email field's autofill on browsers that support it. Nothing to configure: the
relying party is the host of `SITE_URL`, which must be the exact origin users
visit. Verification uses `ring` (already present through rustls), supports
ES256, RS256 and EdDSA, requires user verification, and stores single-use
challenges in Postgres. See [the passkeys guide](app/content/docs/passkeys.md).

## Two-factor authentication

Signed-in users turn on an authenticator app (TOTP, RFC 6238) in account
settings: confirm the password, scan the server-rendered QR code, enter one
code, and save the eight single-use recovery codes shown once. Password
sign-in then stops at a code prompt (five attempts, ten minutes) before any
session exists; passkey sign-in is already two factors and skips it. Nothing
to configure. See [the two-factor guide](app/content/docs/two-factor.md).

## Google sign-in (optional)

The `google` feature (on by default) adds **Sign in with Google** to the
sign-up and sign-in pages once `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`
are set; both blank hides it, one without the other fails startup. Register
`SITE_URL/auth/google/callback` as the OAuth client's redirect URI. Verified
Gmail and Workspace users get an account with one click; everyone else signs up
by email and connects Google from account settings after a password check.
Google never links to an existing account by email. Accounts created with
Google have no password and confirm sensitive settings with a fresh Google
round trip instead; an enrolled authenticator app still applies. ID tokens are
verified with `jsonwebtoken` against Google's cached signing keys; only
`openid email` is requested and no Google tokens are kept. See
[the Google sign-in guide](app/content/docs/google-sign-in.md).

## GitHub sign-in (optional)

The `github` feature (on by default) adds **Continue with GitHub** next to
Google once `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET` (from a GitHub OAuth
App) are set; both blank hides it, one without the other fails startup. Set
the OAuth App's callback URL to `SITE_URL/auth/github/callback`. People whose
primary GitHub email is verified get an account with one click; everyone else
signs up by email and connects GitHub from settings. Accounts are matched by
GitHub's numeric user id, so a renamed login or changed email keeps the same
account. Only `user:email` is requested and the access token is revoked right
after reading the id and email. Both providers share `app/src/social_login.rs`,
which is also where you add another one. See
[the GitHub sign-in guide](app/content/docs/github-sign-in.md).

## Background jobs

Durable jobs live in the `jobs` table of your own Postgres database; no
Redis or extension is needed. Define a job in `app/src/jobs.rs` (a serde
struct implementing `app_core::jobs::Job` with a stable `KIND`), register
it in `jobs::registry()`, and enqueue it with `app_core::jobs::enqueue`,
preferably inside the transaction that creates the data it belongs to. The
shipped example sends a welcome email after signup verification.

A worker claims jobs with `FOR UPDATE SKIP LOCKED`, keeps a 60-second lease
alive with heartbeats, retries failures with exponential backoff up to
`MAX_ATTEMPTS`, and marks the rest dead for `cargo run -- jobs retry <id>`.
Handlers run at least once, so make them idempotent. `JOBS_WORKER` chooses
whether a process serves HTTP, runs jobs, or both; `cargo run -- worker`
runs a worker-only process from the same binary. `cargo run -- jobs stats`
shows the queue. See [the background jobs guide](app/content/docs/background-jobs.md)
for when to use a job instead of the handler, `blocking::run` or an interval.
With the CapRover workflow, adding a `WORKER_APP_TOKEN` secret deploys a
second `-worker` app from the same image; [the CapRover guide](app/content/docs/caprover.md)
has the steps.

## Terms and Privacy

`/terms` and `/privacy` are linked from the footer and listed in the sitemap.
They are a starting point, not legal advice. Before launch, set `COMPANY`,
`CONTACT_EMAIL`, `GOVERNING_LAW` and `UPDATED` at the top of
`app/src/views/legal.rs`, read both pages against what your product actually
does, and have them reviewed. The Privacy Policy lists Stripe, PostHog, Sentry
and file storage only when those features are compiled in; add any other
service you send personal data to.

## About and Contact

`/about` and `/contact` are linked from the footer, listed in the sitemap and
marked up as an `AboutPage` and a `ContactPage` for search engines. Search
engines and AI assistants read these pages to check that a real business
stands behind a product before recommending it. Set `FOUNDER` and `LOCATION`
at the top of `app/src/views/company.rs` and replace every bracketed
placeholder with concrete, true details: who you are, why the product exists
and who it is for. Both pages reuse `COMPANY` and `CONTACT_EMAIL` from
`legal.rs`.

## Working on it

See [AGENTS.md](AGENTS.md): layout, rules, and what "done" means.

## License

Commercial. See [LICENSE.md](LICENSE.md).

## Docker dependency cache

The Dockerfile uses pinned [cargo-chef](https://github.com/LukeMathWalker/cargo-chef)
to compile dependencies in a separate layer. Local Docker builds reuse that
layer; the CapRover workflow exports it with `cache-to: type=gha,mode=max`
so fresh GitHub runners can restore it too. No cache mounts are required.
Source or deployment revision changes rebuild the application while reusing
compiled dependencies. Manifest, lockfile, build profile, or Rust toolchain
changes can invalidate the dependency layer. The first build also installs
cargo-chef; subsequent builds cache that installation.

## Blog

The optional `blog` Cargo feature is enabled by default. It serves `/blog`,
`/blog/{slug}`, and `/blog/rss.xml`, adds navigation and sitemap entries, and
includes a welcome post you can replace. Markdown is rendered once at startup.

The generation choice sets `app/Cargo.toml`'s `[features] default` to the selected `blog` and `docs` features. With the feature disabled, blog routes, navigation, embedded posts,
and RSS are excluded. Markdown/TOML dependencies remain if Docs is enabled.
The source stays in the project so you can enable it later by
adding `"blog"` to the default feature list. For a temporary override, use `cargo run --no-default-features`
or `cargo run --features blog`.

Add a file under `app/content/blog/`:

```markdown
+++
title = "Your post title"
slug = "your-post-title"
description = "A short summary for readers, search results, and RSS."
author = "Your team"
date = "2026-10-06"
draft = true
+++

Your opening paragraph.

## A section heading

Write Markdown here, including links, lists, images, and fenced code blocks.
```

The title becomes the H1; start body headings at H2. Slugs must be unique
lowercase letters/digits separated by single hyphens. Keep published slugs
stable. Quote dates as `YYYY-MM-DD`. Raw HTML is displayed as text.

Set `draft = false` to publish; omitting it also means published. Drafts are
excluded from public pages, RSS, and the sitemap. Dates are metadata, not a
scheduler: a future date does not delay publication. Invalid metadata or
duplicate slugs fail the content tests and app startup when the blog is enabled.

Run the checks in AGENTS.md and preview locally before publishing. Posts are
embedded in the binary, so every edit requires a rebuild and deployment.
Replace or remove `welcome.md` before launching your product.

## Markdown documentation

Published guides also generate `/llms.txt`, a Markdown navigation index linking
to each guide and `/llms-full.txt`, the complete documentation. Both are cached
at startup and exclude drafts. The index must stay below 30,000 characters;
shorten descriptions or split navigation into linked section indexes if needed.

Docs live in `app/content/docs/`. Add a `.md` file with TOML front matter:

```toml
+++
title = "Getting started"
slug = "getting-started"
description = "A short summary of this guide."
section = "getting-started"
order = 1
+++
```

Write Markdown below the closing delimiter. Pages appear at `/docs/{slug}`;
the required published `welcome` page renders at `/docs`, and its old URL
redirects there. The sidebar groups pages by `section`: `getting-started`,
`features`, `deployment`, then `developer-tools`. Empty sections are hidden.
Within each section, pages follow `order` (then slug for ties).

Starter guides cover local setup, Blog, Docs, Sentry, PostHog, Stripe, Qdrant,
Docker, and CapRover. They remain available when optional capabilities are off, to explain
how to enable them. The CapRover guide applies only when its workflow is present.
Replace the starter content with your product documentation before launch.
Set `draft = true` to hide a page. Slugs use lowercase letters and digits
separated by single hyphens. Missing metadata, invalid slugs, empty bodies,
and duplicate slugs fail validation. Raw HTML is displayed as text.

Content is embedded, validated, and rendered once at startup. Run the checks,
then rebuild and deploy to publish edits. Published pages have canonical URLs,
descriptions, and sitemap entries.

The `docs` Cargo feature defaults on independently of `blog`. Remove `"docs"`
from the default feature list in `app/Cargo.toml` to omit docs routes, navigation,
content, and sitemap entries. Markdown dependencies are omitted when both
content features are disabled. Test all four combinations: defaults,
`--no-default-features`, `--no-default-features --features blog`, and
`--no-default-features --features docs`.

## Optional PostHog

PostHog is optional. The generator's **Use PostHog** switch starts on and reveals four
independent capabilities, initially all selected. Uncheck any you do not need.
The selected Cargo features become the app's defaults:

| Feature | What it sends |
| --- | --- |
| `posthog-web` | Browser pageviews, pageleaves, and Core Web Vitals |
| `posthog-product` | Successful signup, sign-in, and sign-out events; UUID identity, email, and account creation date |
| `posthog-logs` | Batched structured Rust application logs |
| `posthog-traces` | Sampled HTTP request spans, with trace context propagation |

Set `POSTHOG_PROJECT_TOKEN` to **your project's ingestion token** in `.env` or
your deployment environment. Set `POSTHOG_HOST=https://eu.i.posthog.com` for an
EU project; the default is US. Never use a personal API key. A blank token
keeps all exports and browser SDK loading disabled. Keys are not collected by
the generator or included in downloads.

You can change the defaults in `app/Cargo.toml` later, or try a capability:

```sh
cargo run --features posthog-web,posthog-product
```

When all PostHog features are disabled, there is no PostHog middleware or
PostHog/OpenTelemetry runtime dependency. Web analytics alone does not need a
Rust SDK. Product events add `posthog-rs`; logs and traces add
OpenTelemetry. The `posthog`, `posthog-server`, and `posthog-otlp` features are
internal groups; select the four capabilities above instead.

The browser SDK loads asynchronously without a JavaScript build step. Replay,
autocapture, exception capture, heatmaps, surveys, and feature flags are disabled.
PostHog error tracking is not used; select Sentry errors for error reporting. Browser and
server identity use the user's UUID; sign-out/account switches reset browser
identity. Product events intentionally include account email. Server telemetry
omits passwords, cookies, bodies, query strings, and raw unmatched paths;
error messages and explicitly logged fields can contain application data, so
review custom instrumentation and configure consent before collecting data
where required.

`POSTHOG_TRACE_SAMPLE_RATIO` defaults to `0.1` and accepts `0` through `1`.
It samples new traces; valid upstream sampled trace context is respected.
Logs are independent of trace sampling. Health checks and static
assets are excluded from request telemetry. Export queues are bounded and
flush at shutdown; request handlers do not wait for telemetry network calls.
An aborting release process can exit before queued exports flush.
To retain source locations in release builds, build with
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and
`CARGO_PROFILE_RELEASE_STRIP=none` (larger binary). Symbol upload is not configured.

Add product-specific events through `state.track(&user, "action succeeded",
serde_json::json!({}))` under `#[cfg(feature = "posthog-product")]`, after the
operation succeeds. Mark the request user with `analytics::record_user(&user)`
under `#[cfg(feature = "posthog")]` before fallible authenticated operations.
Both helpers do no network I/O.

Validation: run `node --test app/tests/analytics.test.cjs`, and run the Rust
checks with `--all-features` plus each `posthog-*` capability on its own. The
collector test checks actual SDK/OTLP payloads locally; it never sends test
accounts to PostHog.

## Sentry (optional)

**Use Sentry** and all five capabilities start on. Expand **Customize Sentry**
to choose the features you need; retain at least one, or turn Sentry off.
The same choices are available through Copier. They select the default Cargo
features in `app/Cargo.toml`, independently of PostHog:

| Cargo feature | What it enables |
| --- | --- |
| `sentry-errors` | Browser exceptions, server errors with source chains, and panic reports |
| `sentry-traces` | Server/browser request traces and supported Chromium browser profiling |
| `sentry-logs` | Structured Rust and browser application logs |
| `sentry-metrics` | Request counts, response times, and browser page-load counts |
| `sentry-replay` | Masked browser session recordings |

Change these defaults in `app/Cargo.toml` to enable or remove capabilities later.
For example, `cargo run --no-default-features --features sentry-errors` runs with
only error tracking. Replay-only builds omit the Rust Sentry SDK; other builds
include only the SDK integrations required by their selected capabilities.
`sentry`, `sentry-server`, and `sentry-layer` are internal dependency groups.

Set your own `SENTRY_DSN` to activate the selected capabilities. A blank DSN
keeps both server and browser SDKs inactive. Set `SENTRY_ENVIRONMENT` for your
deployment. Trace sampling defaults to 1.0 (`SENTRY_TRACES_SAMPLE_RATE`), and
replay and browser profiling each default to 0.25 (`SENTRY_REPLAY_SAMPLE_RATE`
and `SENTRY_PROFILE_SAMPLE_RATE`). All accept 0–1 and apply only when their
capability is enabled. Error-session replay is 100% when both replay and errors
are selected; replay alone uses session sampling. Browser profiling belongs to
traces and can be disabled with a profile sample rate of zero. Rust profiling
is not supported by Sentry.

`app/src/sentry.rs` owns startup configuration and isolated request hubs;
`app/src/observability.rs` shares error reporting and the tracing subscriber
with PostHog. Releases use the Cargo package name and `BUILD_REVISION`. Server
SDK initialization precedes Tokio and shutdown drains its bounded queues.
No request bodies, cookies, query strings, or user profiles are attached.
Browser URL filtering masks UUID path segments; replay masks text/inputs and
blocks media. Review custom messages and logging fields before including
private data. No JavaScript build step is needed.

Run the workspace tests with default features, `--all-features`, and each
capability independently, plus `node --test app/tests/sentry.test.cjs`. The
collectors use local or in-memory transports and verify that unselected
capabilities produce no payloads. They do not send data to a real Sentry project.

## Stripe billing (optional)

Enable the `stripe` feature for hosted Checkout subscriptions and one-time payments,
verified lifecycle webhooks, a billing page, and Stripe's customer portal. Configure
`STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, and either or both
`STRIPE_SUBSCRIPTION_PRICE_ID` / `STRIPE_PAYMENT_PRICE_ID`. No publishable key is needed.
Blank configuration keeps billing unavailable without preventing local development.

Follow [the billing setup guide](app/content/docs/stripe.md) for restricted-key
permissions, required webhook events, portal setup, sandbox testing, access policy,
and the manual refund/dispute and reconciliation boundaries. Product routes can use
`billing::require_access` after `CurrentUser` (or `ApiUser`); authentication stays free.
Admins receive complimentary paid-feature access even without configured Stripe
credentials. Grants and revocations apply on the next request, including existing
API keys. Admin access does not fabricate purchases or cancel subscriptions;
existing customers can still manage billing in the portal. With Stripe disabled,
there are no built-in paid-feature gates.

## Vector search with Qdrant (optional)

Enable the `qdrant` feature to connect the app to a [Qdrant](https://qdrant.tech)
vector database. `app/src/vectors.rs` reads `QDRANT_URL`, `QDRANT_API_KEY`,
`QDRANT_COLLECTION`, `QDRANT_VECTOR_SIZE` and `QDRANT_DISTANCE`, creates the
collection at startup when it is missing, and offers `upsert`, `search`,
`delete` and `delete_all`, each scoped to one user: the owner is written into
every point and every query filters on it. Bring your own embedding model; the
client only stores and searches `f32` vectors over Qdrant's REST API with the
`reqwest` client already in the tree. Blank configuration keeps the app runnable
with `state.vectors` set to `None`; account deletion queues a `purge_vectors`
job. `docker compose --profile qdrant up -d` starts a local server. See
[the Qdrant guide](app/content/docs/qdrant.md).

## Setup API (optional)

The `api` feature (on by default) gives your users a way to interact with your
platform through an API: personal API keys in **Settings** and a JSON API at
`/api/v1`. Keys are bearer tokens, shown once and stored hashed;
`GET /api/v1` describes the service and `GET /api/v1/me` returns the caller's
account. The app is also an OAuth 2.1 authorization server, so applications
and hosted AI assistants can connect with a sign-in and consent instead of a
key (`/.well-known/oauth-authorization-server`, PKCE, rotating refresh tokens,
**Connected apps** in Settings). Add product endpoints in `app/src/api.rs` with the `ApiUser`
extractor; see [the API guide](app/content/docs/api.md). Remove `"api"` from
the default features to drop the routes, the OAuth endpoints and the settings panels.

## MCP server (optional)

The `mcp` feature (on by default; it needs `api`) serves a Model Context
Protocol server at `/mcp` so coding agents such as Claude Code and Cursor, and
hosted assistants such as Claude and ChatGPT, can call the product as tools,
connecting with OAuth or the same API keys. The protocol
core lives in `crates/core/src/mcp.rs` and is upgraded with LVTD;
`app/src/mcp.rs` ships a `get_account` tool and is where product tools go.
See [the MCP guide](app/content/docs/mcp.md). Remove `"mcp"` from the default
features to drop the route and the settings link.

## File storage (optional)

The `storage` feature (on by default) puts user uploads behind one
`app_core::storage::Storage` in the app state. With every `STORAGE_S3_*`
value blank, files go to `STORAGE_LOCAL_DIR` (`./storage`; `/app/storage` in
the image, which a redeploy wipes unless a volume is mounted). Set
`STORAGE_S3_BUCKET`, `STORAGE_S3_ENDPOINT`, `STORAGE_S3_ACCESS_KEY_ID` and
`STORAGE_S3_SECRET_ACCESS_KEY` (plus `STORAGE_S3_REGION` for AWS; R2 uses
`auto`) and the same code writes to Cloudflare R2, AWS S3, MinIO or Backblaze
B2 with path-style, SigV4-signed requests and no SDK. `STORAGE_S3_PUBLIC_URL`
links files straight from the bucket. Profile pictures in **Account settings**
(`app/src/avatars.rs`, migration 0012) are the example to copy for other
uploads; see [the file storage guide](app/content/docs/file-storage.md).
Remove `"storage"` from the default features to drop the upload routes and the
settings panel.

## Account settings

Signed-in users open **Account settings** from the header's account menu. They
can add or clear a name, upload a profile picture (with the `storage` feature),
change their sign-in email with their current password,
and select Light, Dark, or System appearance for this browser. System is the
default and follows live device changes. Email changes sign out other sessions;
the replacement email address is not verified.

With `stripe` enabled, Settings links to Billing for plan status, checkout, and
the customer portal. Account deletion requires the current password and typed
`DELETE`. Pending checkouts and subscriptions that have not finished cancellation
block deletion, including when Stripe is subsequently disabled. Resolve them in
Billing or with an operator first. Deletion removes local account/session/billing
records, does not refund purchases, and leaves provider payment records intact.
Customize the deletion transaction for any additional product data you add.

## Automatic IndexNow

IndexNow is included automatically; no account, API key or scheduled workflow
is needed. Set `SITE_URL` to your actual public HTTPS origin (for example,
`https://your-product.com`). Normal startup creates a random per-origin key in
Postgres and serves `/<key>.txt` as plain text. Local HTTP, IP addresses and
reserved development domains do not submit. Set `INDEXNOW_ENABLED=false` on
public staging/preview deployments or to opt out. Keep `SITE_URL` canonical.

After startup, a background worker checks that the public key endpoint is
serving this build, then notifies IndexNow about the homepage and enabled,
published blog/docs pages. Drafts and account/auth/billing URLs are excluded.
App source, assets, content or feature changes notify the current public page
set; removed pages are also submitted from the previous accepted snapshot.
This deliberately sends all public pages when the build fingerprint changes,
not just pages with a changed publication date. Unchanged restarts send nothing.

The worker retries errors with backoff (up to one hour), checks every 15 minutes
after success, and only advances its durable snapshot after every batch is
accepted. Replica row locks prevent simultaneous submissions. A crash after
acceptance but before saving can repeat a batch safely. Keep the Postgres
database across deploys; deleting its state loses removal history.

Public ownership endpoints must be accessible without authentication, redirects
or proxy caching, including the `x-indexnow-revision` header. The fingerprint
works without Git or a particular hosting provider. Rolling deployments may
delay notifications until the public endpoint matches the worker's build.

For new public route types, extend `indexnow_paths` and the sitemap together.
Content here is embedded at build time: publish edits by rebuilding/deploying.
Future database-backed content needs a publish/update/delete integration; the
startup snapshot does not watch arbitrary database rows.

Successful submission logs `IndexNow URLs accepted`; HTTP 200 means received
and 202 means received with ownership validation pending. Neither guarantees
crawling, indexing or ranking. IndexNow complements the sitemap; it does not
replace Google Search Console. See the [IndexNow protocol](https://www.indexnow.org/documentation).

## Admin panel

Every project includes a read-only user overview at `/admin`. Admin users open it
from **Admin panel** in the navbar account dropdown on any signed-in page. It
lists email, join date, and role with 50 accounts per page. Anonymous visitors
must sign in; ordinary users cannot access the panel.

Existing accounts and new signups are non-admin by default. After creating an
account, grant or revoke admin access from a trusted shell:

```bash
cargo run -- admin grant you@example.com
cargo run -- admin revoke you@example.com
```

Set `DATABASE_URL` for the intended environment. These commands apply pending
migrations and fail if the account does not exist. In a deployed container, use
`lvtd-dev admin grant <email>` (or `revoke`) with the app's runtime environment.
Roles are read on each request, so changes affect existing sessions immediately.
Admin access is independent of billing and works with every optional feature
disabled. Public signup and profile forms cannot grant admin access.

To extend the panel, keep administrative routes authenticated with `CurrentUser`
and call `admin::require_admin(&user)` before reading or changing privileged data.

## Mutation testing

CI uses `cargo-mutants` to check whether tests catch deliberate changes to Rust
code. Pull requests and main pushes check changed code; full audits are available
locally and through the manual CI `full_mutation` input. See the
[mutation-testing guide](docs/mutation-testing.md).

## Fuzz testing

Every pull request runs coverage-guided fuzz tests with libFuzzer and
AddressSanitizer. Install nightly Rust and `cargo-fuzz`, then run
`bash scripts/fuzz.sh 30` from this workspace's root. See [fuzz/README.md](fuzz/README.md)
for targets, longer campaigns, crash replay, minimization and regression tests.

## Search and social previews

Every published public page has its own title, description, canonical URL and
1200×630 PNG for Open Graph and X cards. The homepage and legal cards work even
with blog and docs disabled. Cards render at startup and use your project name;
URLs change when the image changes. Set `SITE_URL` to your public origin and
customize `HOME_TITLE`, `SITE_DESCRIPTION` and `SITE_NAME` in `app/src/views/mod.rs`.
Legal descriptions live in `views/legal.rs`, About and Contact in `views/company.rs`;
blog and docs use their front matter.

`/robots.txt` advertises the XML sitemap. Public pages allow large image previews;
account pages and errors emit `noindex, follow`. When adding a marketing page,
register its metadata in `structured_data::build`, its card in `og::load`, and
its URL in both the sitemap and `indexnow_paths`. The sitemap regression checks
every public page's metadata and fetches its actual image.

## Structured data

Public pages emit JSON-LD in their initial HTML: a `SoftwareApplication` identity,
`WebSite`, and page metadata (`BlogPosting` for posts and `TechArticle` for guides).
The JSON is rendered at startup, with no browser script or per-request serialization.
Only published content is included; private pages and errors have no JSON-LD.

Set `SITE_URL` to your public origin and customize `SITE_NAME` and `SITE_DESCRIPTION`
in `app/src/views/mod.rs` alongside your landing-page copy. The generator sets the
name for you. Edit `identity` in `app/src/structured_data.rs` to add actual `offers`
(price, currency and purchase URL) and official `sameAs` profile URLs. If your site
represents a company or person instead of software, change its `@type` to
`Organization`, `LocalBusiness`, or `Person` and replace the software-specific fields.
Blog author names come from front matter; add an explicit `Person` or `Organization`
type and profile URL when you know which applies. No prices, ratings, company details
or social profiles are fabricated by the template.

### Markdown for agents

Public pages serve Markdown at their normal URL when a client sends
`Accept: text/markdown`. Browsers continue to receive HTML. Both responses carry
`Vary: Accept`. Landing pages, legal pages, and enabled blog/docs pages all use
this shared behavior. Register additional public pages with `public_page` in
`app/src/routes.rs`; the HTML view is the only content to maintain. Markdown is
prepared once at startup, with scripts and navigation omitted.

```bash
curl -sS -L -i -H 'Accept: text/markdown' http://localhost:3000/
curl -sS -L -i -H 'Accept: text/html' http://localhost:3000/
```
