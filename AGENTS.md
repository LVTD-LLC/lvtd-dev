# AGENTS.md — operating manual for LVTD

LVTD is a SaaS starter in Rust. This file is the contract for anyone,
human or agent, changing it. Read it fully before editing.

## What this is

A server-rendered web app: Axum (HTTP), Maud (HTML, compile-time checked),
Postgres via sqlx (compile-time checked SQL), one static binary. Accounts
are email + password with server-side cookie sessions.

**Performance is the primary goal.** Do not add per-request work that can be
done once at startup or at build time. No JavaScript build step.

## Layout

```
app/                 The product. Edit freely.
  build.rs             rebuild when Markdown content changes
  content/blog/        Markdown posts with TOML front matter
  src/blog.rs          optional blog parsing, validation, startup rendering, RSS
  src/main.rs          CLI: serve (default) | migrate | admin grant/revoke
  src/lib.rs           AppState, build()
  src/routes.rs        router + handlers
  src/settings.rs      profile changes, appearance, and password-confirmed account deletion
  src/avatars.rs       optional profile pictures: the example of file storage (crates/core storage)
  src/admin.rs         admin authorization, operator role commands, paginated account overview
  src/api_keys.rs      optional personal API keys: the `api_keys` table, creation, revocation, bearer lookup
  src/api.rs           optional JSON API at /api/v1: ApiUser extractor, JSON errors, shared endpoints,
                       generated OpenAPI document (/openapi.json and /api/v1/openapi.json) and Swagger UI (/api/v1/docs)
  src/mcp.rs           optional MCP server at /mcp: your tools; protocol in crates/core/src/mcp.rs
  src/oauth_server.rs         optional OAuth 2.1 HTTP surface: metadata, registration, token/revoke endpoints, consent page,
                       sign-in return trip; protocol and SQL in crates/auth/src/oauth_server.rs
    views/oauth_server.rs     the consent and refusal pages
  src/verification.rs  signup codes (`auth`), mail transport (Mailgun, or the log locally), pending state and send budgets
  src/passkeys.rs      passkey (WebAuthn) JSON handlers; assets/passkeys.js drives the browser
  src/two_factor.rs    authenticator-app (TOTP) enrolment on /settings/two-factor and the sign-in code prompt
  src/social_login.rs  optional sign-in providers: sign-up/sign-in, linking from settings, session-bound confirmation
    social_login/google.rs  Google client: authorization URL, code exchange, cached ID-token verification
    social_login/github.rs  GitHub client: OAuth App code exchange, user id and primary email, token revocation
  src/jobs.rs          background job kinds and their registry (example: welcome email)
  src/billing.rs       optional hosted Stripe Checkout, subscriptions, portal and signed webhooks
  src/vectors.rs       optional Qdrant vector search: runtime Config, REST client, per-user scoping
  src/errors.rs        Markdown 404 body; uses core HTTP content negotiation
  src/og.rs            per-page social cards for blog and docs: rendered once per process, served at /og/…
  src/structured_data.rs startup-rendered JSON-LD identity and public page metadata; no private routes
  src/views/           maud templates (mod.rs = layout, pages.rs = pages; home() is the landing page)
    api_docs.rs        /api/v1/docs Swagger UI page (pinned jsDelivr bundle with SRI; assets/api-docs.js starts it)
    company.rs         /about and /contact; owner fills FOUNDER, LOCATION and the bracketed copy.
    legal.rs           /terms and /privacy; owner fills COMPANY, CONTACT_EMAIL, GOVERNING_LAW, UPDATED.
                       Third-party services are listed per enabled feature; keep that in sync.
  src/assets.rs        embedded /assets/* with immutable caching
  assets/              style.css, favicon.svg, og-image.png (embedded, content-hashed URLs)
  og-image/            og.html source and render.sh for assets/og-image.png, the link-preview image
  tests/app.rs         end-to-end tests through the router, real Postgres
crates/core/         Config, Pool (applies the app's migrator), AppError, middleware, tracing, bounded CPU work,
                     jobs (durable Postgres queue: enqueue API, worker, operator queries),
                     mcp (Model Context Protocol: stateless HTTP transport, JSON-RPC, tool results),
                     storage (user uploads: S3-compatible bucket or local disk, hand-signed SigV4, no SDK),
                     og (`og` feature: 1200×630 card PNGs with embedded Inter, tiny-skia + skrifa + harfrust)
crates/auth/         users, passwords (argon2id), sessions, extractors, passkeys (WebAuthn), TOTP two-factor,
                     oauth (an OAuth 2.1 authorization server: clients, PKCE codes, rotating tokens, revocation)
migrations/          sqlx migrations, embedded by app/src/lib.rs (MIGRATOR), run at startup
.sqlx/               sqlx offline query metadata. MUST be regenerated when SQL changes.
_hosting/<host>/     optional files overlaid onto the root when that host is chosen;
                     never shipped as-is. caprover/ = GitHub Actions → GHCR → CapRover.
```

## Landing page

`views::pages::home()` ships as placeholder copy in the shape a landing page
should keep. It is read by the product's customers, not by developers, so when
rewriting it:

1. **One idea per screen.** Each `section class="screen"` is one full-height
   screen with one headline (`.screen-title`, optional `.screen-title-soft`
   second line) and one short supporting paragraph (`.screen-sub`). A second
   idea gets its own screen; do not add grids of cards or lists of benefits.
2. **Headlines a fifth grader understands.** Plain words about what the
   customer gets. No stack names, acronyms or internal jargon on public pages.
3. **Real numbers, with their source.** Prefer a measured figure to an
   adjective, and put how it was measured in small print under it.
4. **One action.** The hero and the last screen point to the same call to
   action.

Keep exactly one `h1` (the hero); every other screen title is an `h2`. When the
headline changes, update `HOME_TITLE` and `SITE_DESCRIPTION` in `views/mod.rs`
to match; the generated homepage card updates automatically.

## Hosting

The template deploys as one Docker image (`Dockerfile`). Every response
carries `x-deployment-revision` (from the `BUILD_REVISION` build-arg) so a
pipeline can confirm the new build is live via `/healthz`. Choosing a host
at generation time adds its pipeline from `_hosting/<host>/`; `none` adds
nothing. Adding a host = a new `_hosting/<host>/` directory + a choice in
`copier.yml`, which lives one level up at the repo root next to the
generator site that serves it. Host files must only reference the
project by the `lvtd-dev` identifier so generation renames them like
everything else.

The split matters for upgrades: `crates/*` is upgraded in place by LVTD
releases; `app/` is yours. Put product logic in `app/`, put reusable
infrastructure in a crate.

## Optional blog

The app's `blog` Cargo feature defaults on. It gates the blog modules, state,
routes, navigation, RSS, and sitemap entries. Markdown/TOML dependencies are
shared with docs and omitted only when both features are disabled.
An app manifest with `default = []` runs without a blog. Keep both modes
working; test with `cargo test --workspace` and
`cargo test --workspace --no-default-features`. See README for publishing.

Core and auth dependencies use stable `app-core` and `app-auth` aliases; their
generated package names are `core` and `auth`. The binary and integration tests
import the product crate as `app` in a separate
import group. Keep that alias stable so generated project names do not change
Rust formatting. Fixed cryptographic fixture bytes must not contain renameable
branding; encode them as hex like the signature and key.
Use `crate::views::SITE_NAME` for runtime branding (including emails); the
generator fills that constant with the chosen project name.
Every page emits a description, Open Graph tags and a large X card using
`views::SITE_DESCRIPTION` unless the handler passes its own description.
Every public page gets a generated card (`src/og.rs`, keyed by page path),
including the homepage and legal pages without blog/docs features. The cards use
`SITE_NAME`, so downloaded projects carry their own name. `assets/og-image.png`
is a fallback for unregistered pages; keep its source and `OG_IMAGE_ALT` in step.
Public pages registered in `structured_data::build` allow large search previews;
other layout pages emit `noindex, follow`. Register new marketing pages there,
in the sitemap and IndexNow, and in the card list. `/robots.txt` advertises the sitemap.
`views/attribution.rs` is generator provenance and ships verbatim. The HTML
generator tag and optional footer link use it; app owners may remove the link
from `views/mod.rs`.

## Rules

1. **All SQL goes through `sqlx::query!` / `query_as!`** so it is checked at
   compile time. After changing any query or migration run
   `cargo sqlx prepare --workspace` and commit `.sqlx/`. CI fails otherwise.
2. **Schema changes are new migration files** (`migrations/NNNN_name.sql`),
   never edits to existing ones.
3. **Handlers return `Result<_, AppError>`.** Use `AppError::Status` for
   expected failures; anything else is logged and becomes a 500.
4. **Signed-in-only routes take `CurrentUser`** as an argument. That is the
   whole authorization mechanism; do not check cookies by hand.
5. **Forms post to the same path they are served from** and re-render with
   the error and the user's input on failure (422). Redirect on success.
6. **Passwords never touch a request thread.** Hashing goes through
   `app_auth::password`, which uses `app_core::blocking::run`. Its process-wide semaphore
   allows two CPU jobs at a time and holds capacity until each blocking
   closure finishes, even after request cancellation.
7. **No secrets in the repo.** New config goes in `Config`, `.env.example`
   and this file's layout section if it adds a module.
8. **Every change ships with a test** in `app/tests/app.rs` (or a unit test in
   the crate). Tests run against a fresh Postgres database each.
9. **Dependencies are a liability for every generated project.** Before adding
   a crate, use `std` or a crate already in the tree. Prefer actively released
   ecosystem crates (tokio/tower/axum, sqlx, serde, RustCrypto, dtolnay's) and
   enable only the cargo features that are used. Never pull in a crate for a
   few lines of code you could write. `cargo deny check` (`deny.toml`) must
   pass: no known vulnerabilities, permissive licenses, crates.io only.

## Commands

```
docker compose up -d                   # Postgres 18 on :5433
cp .env.example .env
SQLX_OFFLINE=true cargo run            # first run: DB has no tables until startup migrates
cargo run                              # http://localhost:3000
cargo test --workspace                 # needs DATABASE_URL
cargo sqlx prepare --workspace         # after SQL changes
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check                       # cargo install cargo-deny --locked
```

## Done means

`cargo fmt --all --check`, `cargo clippy ... -D warnings`,
`cargo sqlx prepare --workspace --check` and `cargo test --workspace` all pass.
After dependency changes, `cargo deny check` passes too. Also run `SQLX_OFFLINE=true cargo check --workspace --all-targets`.
For blog changes, run clippy, tests, and offline checks with
`--no-default-features` too.

## Optional docs

`app/content/docs/` holds Markdown guides with TOML title, slug, description,
section, order, and optional draft metadata. The published `welcome` page is
required and renders at `/docs`; `/docs/welcome` redirects there. Group guides
into Getting started, Features, Deployment, and Developer tools. Empty sections
are hidden. Common setup guides remain available for disabled capabilities,
and the CapRover guide must explain that its workflow is optional. `app/src/docs.rs` validates and renders
content at startup; `app/src/views/docs.rs` renders the index and sidebar.
`/llms.txt` (`Docs::llms_txt`) is a navigation index under 30,000 characters,
with links to published guides and `/llms-full.txt` (`Docs::llms_full_txt`),
which combines their Markdown in sidebar order. Both are cached in `AppState`
once at startup; oversized indexes fail validation. Never hand-maintain them.
The template's independent `docs` Cargo feature defaults on alongside `blog`.
Check all four feature combinations with clippy, tests, and offline compilation.
Markdown dependencies remain if either feature is enabled.

## Optional PostHog

`app/src/analytics.rs` reads PostHog Config once at startup and records optional
product events. `app/src/observability.rs` owns bounded OTLP exporters and the
HTTP error boundary; `app/assets/analytics.js` defers browser SDK loading.
`app/src/errors.rs` preserves source errors with `sentry-errors` enabled.
PostHog error tracking is not used: never enable PostHog exception capture.
Keep provider-specific code in the app, not the reusable auth/core crates.

The four capabilities are `posthog-web`, `posthog-product`, `posthog-logs`,
and `posthog-traces`. All are off in this source workspace;
generation selects Cargo defaults. A blank runtime `POSTHOG_PROJECT_TOKEN`
disables exports. Never ship site-specific events, service names, or tokens.
Product events include email; do not add passwords, cookies, bodies, or query
strings to telemetry.

Run the five done checks with default features and `--all-features`, and clippy,
tests, and offline checks for each capability independently with
`--no-default-features --features <capability>`. Also run
`node --test app/tests/analytics.test.cjs`. The collector test validates actual
SDK/OTLP payloads for the selected subset, including absence of unselected data.

## Optional Sentry

The five capabilities `sentry-errors`, `sentry-traces`, `sentry-logs`,
`sentry-metrics`, and `sentry-replay` all default on and work independently of
PostHog. `sentry`, `sentry-server`, and `sentry-layer` are internal dependency
groups, not user-facing choices. Replay alone needs only DSN validation types,
not the Rust SDK. Only errors add panic/source-error integrations; only logs add
the SDK log feature, and only metrics add the SDK metric feature.

`app/src/sentry.rs` owns Config, request isolation, traces, metrics, and the log
layer; `app/assets/sentry.js` loads the browser SDK. A blank `SENTRY_DSN` disables
both SDKs. Parse configuration once before Tokio starts. Ignore sample-rate
settings for disabled capabilities. Browser profiling belongs to traces.
Keep provider code in the app and use package name + deployment revision for
releases. Preserve source errors with `sentry-errors`.

Never attach bodies, cookies, query strings or user profiles; keep replay
masked. Browser SDK v11 uses `dataCollection` for those privacy settings and
`beforeSendLog` / `beforeSendMetric` to filter disabled payloads. Turning errors
off must omit default browser error handlers and filter error events.

Run the five checks with default and all features; clippy, tests, and offline
compilation for each individual capability with `--no-default-features`; and
`node --test app/tests/*.test.cjs`. Collectors must verify absence of unselected
payloads as well as errors/logs/traces/metrics, isolation and request privacy.

## Optional Stripe

`app/src/billing.rs` owns Config, the optional Stripe client, checked billing SQL,
hosted Checkout, portal sessions, signature verification and webhook fulfillment.
The `stripe` feature is off in this source workspace. Credentials are runtime-only;
see `.env.example` and `app/content/docs/stripe.md`. No browser SDK is needed.
`migrations/0003_billing.sql` adds isolated billing tables even in disabled builds.

Browser billing routes require CurrentUser and the origin guard. Only the signed
`POST /stripe/webhook` bypasses origin checks. Access is never granted by a redirect.
Preserve durable attempts, idempotency and account locks before canonical retrieval.
A one-time purchase grants permanent access; trialing subscriptions and active subscriptions with a paid latest invoice grant
access until their status changes. An active admin role also grants access,
without a purchase. Product routes (including API handlers) opt into
`billing::require_access`; use `BillingStatus::has_access()` for entitlements,
not the `paid` field, which only records actual one-time purchases.
Refund/dispute entitlement handling is manual; do not imply it is automated.

Run the done checks plus clippy, tests and offline checks with `--features stripe`,
`--no-default-features --features stripe`, and `--all-features`. Prepare metadata
with `cargo sqlx prepare --workspace -- --all-features --all-targets`. Tests use
local HTTP doubles and signed webhooks, never live Stripe accounts.

## Optional Qdrant

`app/src/vectors.rs` owns `Config` (`QDRANT_URL`, `QDRANT_API_KEY`,
`QDRANT_COLLECTION`, `QDRANT_VECTOR_SIZE`, `QDRANT_DISTANCE`), the REST
`Client` and the owner scoping. The `qdrant` feature is off in this source
workspace; it enables only `reqwest/json`. Never add the Qdrant SDK or a gRPC
stack for this: the API surface is five endpoints and the client is a page.
All blank disables (`state.vectors` is `None`); partial configuration fails
startup, and `ensure_collection` refuses a collection whose vector size or
distance differs from the configuration instead of silently mixing models.

Every public method takes the owner's user id first. `upsert` overwrites the
`user_id` payload key, `search`/`search_filtered` always add the owner
condition to `must`, and `delete` combines it with `has_id`, so no caller can
read or remove another user's points. Keep that invariant when adding
methods. Payloads hold display fields and Postgres ids, never emails or
private text. Point ids are UUIDs. Account deletion enqueues
`jobs::PurgeVectors` inside the deleting transaction; the handler is a
no-op when Qdrant is not configured in that process. Embeddings are the
product's concern: the module never calls a model.

Tests (`app/src/vectors/tests.rs`) run against an in-process double of the
REST endpoints and never contact a real server. Run the done checks plus
clippy, tests and offline checks with `--features qdrant`,
`--no-default-features --features qdrant` and `--all-features`.

## Account settings

`/settings` requires `CurrentUser` for GET and POST. The app owns the profile
name (migration 0004); the shared auth crate stays unchanged. Email changes
require the current password and revoke other sessions in the same transaction.
Checkout attempts retain their original email (migration 0005) for Stripe
idempotency after profile changes. Replacement addresses are not email-verified.
Passwords are never re-rendered.
Appearance is per browser: Light, Dark, or System, with System as the default.
The account dropdown links to Admin for admins, Settings, and posts sign-out; Docs and Blog links
respect their feature flags. Billing links use the optional existing `/billing` flow.

Deletion requires the current password and typed DELETE. It locks the user and
billing account, blocks active checkouts and nonterminal subscriptions (including
scheduled cancellations), and cascades local records. These safeguards apply even
when Stripe is disabled after earlier use. Resolve payments/cancellations before
deletion; provider records remain and no refunds are issued. Run the standard
workspace checks, all four content feature combinations, Stripe modes, and
`node --test app/tests/*.test.cjs` when changing these flows.

To prepare shared-crate test queries as well, pass the Cargo workspace flag too:
`cargo sqlx prepare --workspace -- --workspace --all-features --all-targets`.

## Optional accounts

The `auth` Cargo feature defaults on. It gates every account surface in the
app: the sign-up, verification, sign-in, sign-out, passkey and two-factor
routes, `/dashboard`, `/settings`, `/admin` (modules `admin`, `passkeys`,
`settings`, `two_factor`, `verification` and their views), the `auth_origin`
guard, the navbar's account menu and sign-in buttons, `AppState::email` and
`AppState::rp`, the `admin` CLI command, the session and verification cleanup
intervals and the `SendWelcomeEmail` job. `google`, `github`, `api`, `mcp`,
`storage`, `stripe` and `qdrant` act for a user and turn `auth` on through their
own features.

Without it the app is a public site: the homepage call to action and the legal
pages follow `cfg!(feature = "auth")`. The `app-auth` crate stays a dependency
(`MaybeUser` and `User` remain in page signatures; with no session cookie the
extractor never queries) and every migration still runs, so turning accounts
on later needs no schema work. Gate new account code on `auth`, and keep
public pages working without it. Run the done checks with
`--no-default-features` and `--no-default-features --features auth`.

## Email verification

Every build with accounts requires a browser-bound six-character email code before creating a
new user or session. `app/src/verification.rs` owns provider configuration and
pending state; migration 0006 is additive. Public signup must never call
`users::sign_up` directly. The shared auth crate remains provider-independent.

Runtime-only `MAILGUN_SENDING_KEY`, `MAILGUN_DOMAIN`, `MAIL_FROM`, and independent
`EMAIL_VERIFICATION_KEY` configure Mailgun US. With the three Mailgun values blank,
`verification::Client::from_env` selects the console transport, which writes every
message (codes included) to the log at WARN and uses a fixed development hash key
when `EMAIL_VERIFICATION_KEY` is blank; it refuses to start on an HTTPS `SITE_URL`.
Partial Mailgun configuration fails startup. Outside the console transport, do not
log codes, tokens, recipients, passwords, provider bodies or credentials. Never add
a flag that skips verification; the console transport is the development path.
Never bypass verification when configuration or delivery fails. Existing accounts
remain usable.

Codes expire after ten minutes, allow five guesses, and are replaced on resend
with a 60-second cooldown. Durable budgets are five per address and 200 globally
per hour. Browser token binding rotates when a pending signup is replaced.
Only keyed code hashes, hashed tokens, and Argon2id passwords are stored.
Verification locks and consumes pending state atomically with account creation.
Strict Origin checks protect all three signup/verification POST routes.
Hourly cleanup removes pending state and send buckets older than a day.

Run the done checks, no-default-features and all-features checks, and
`node --test app/tests/*.test.cjs`. Tests use local HTTP doubles, never real mail.
Keep the setup guide and `.env.example` current. Replacement email addresses
in account settings remain outside this signup verification flow.

## Passkeys

`crates/auth/src/webauthn.rs` is pure WebAuthn verification (CBOR, COSE keys,
ES256/RS256/EdDSA through `ring`, which rustls already provides; never add
OpenSSL or a WebAuthn framework for this). `crates/auth/src/passkeys.rs` owns
migration 0011's `passkeys` and `passkey_challenges` tables, options generation
and both ceremonies. `app/src/passkeys.rs` holds the four JSON handlers and
`app/assets/passkeys.js` the browser side; settings forms post to `/settings`
with `passkey` (no-JavaScript fallback, 422) and `remove_passkey` actions.

Passkeys never create accounts. Registration requires `CurrentUser` plus the
current password, requests discoverable credentials with user verification
required and `attestation: "none"`, and is limited to ten per account. Sign-in
is anonymous, requires the user-verified flag, checks the challenge, origin,
rp id hash, signature counter and user handle, and ends in the same session
cookie as the password form. Challenges are 32 random bytes in Postgres,
single-use, bound to the user for registration, ten-minute expiry, swept on
issue. The relying party comes from `SITE_URL` (`AppState::rp`); no new
environment variables. The four `/passkeys/` POST routes are in the strict
`auth_origin` list. Tests use `app_auth::testing::Authenticator`
(feature `test-support`, dev-dependency only); never enable that feature in a
release build. Run both workspace checklists, every template feature mode, and
regenerate SQLx metadata in both workspaces when the auth crate's SQL changes.

## Optional sign-in providers (Google, GitHub)

The `google` and `github` Cargo features default on; each turns on the internal
`social-login` feature, which depends on `auth`. `app/src/social_login.rs` owns,
for every provider alike, `POST /auth/{id}`, `GET /auth/{id}/callback`,
`POST /settings/{id}` and `?{id}=…` notices, the `social_identities` (one per
provider per user, unique `(provider, subject)`), `social_login_flows` and
`social_login_confirmations` tables and `users.passwordless` (migration 0016,
present in every build; it replaced 0014's Google-only tables and copied their
identities). A provider is a `social_login::Client` (authorization URL plus
code → `Identity { subject, signup_email }`) listed in `social_login::SUPPORTED`
and configured in `Providers::from_env`. Adding one takes a client, a Cargo
feature that turns on `social-login`, those two entries and a button icon in
`views::pages`; never a migration.

`social_login/google.rs` is OpenID Connect (`jsonwebtoken` over the `aws-lc-rs`
already in the tree; never add an OAuth framework for this): only `openid email`,
RS256 tokens signed by a cached Google key (refreshed at most every 30 seconds,
at most 32 keys) with issuer, audience, `azp`, expiry, `iat` within ten minutes
and the nonce checked. Only verified Gmail or Workspace (`hd`) addresses create
accounts (`authoritative_email`). `social_login/github.rs` is an OAuth App with
S256 PKCE and only `user:email`; its subject is the numeric user id (never the
renameable login), only a verified primary, non-noreply email creates accounts,
and the access token reads `/user` and `/user/emails` and is then revoked.

Set each provider's `{ID}_CLIENT_ID` and `{ID}_CLIENT_SECRET` together or leave
both blank: blank hides its button (routes answer with an `unavailable` notice),
partial configuration fails startup, and `SITE_URL` must be HTTPS or localhost.
Register exactly `SITE_URL/auth/{id}/callback`. The guides are
`app/content/docs/google-sign-in.md` and `app/content/docs/github-sign-in.md`.

Rules: no provider tokens are stored. Flows are browser-bound, single-use,
ten-minute rows holding hashes of the state and browser tokens plus the nonce
and S256 verifier; callbacks consume the row before the code is exchanged.
Provider bodies are bounded and never logged. An email that matches an existing
account never links by itself. Passwordless accounts keep a hash of a discarded
random password, so the shared auth crate and its schema stay untouched, and
prove ownership for sensitive settings through `settings::prove_owner`, which
accepts a confirmation through any linked provider on the same session for five
minutes instead of a password. Keep every sensitive action on that helper. They
can link a second provider after confirming, and cannot disconnect their last.
Linking and confirming re-check the user and session after the round trip under
a row lock; disconnecting deletes flows and two-factor challenges and revokes
other sessions, and `issue_login` re-checks the identity after creating a
session or challenge so a racing disconnect can never hand out a credential.
Provider sign-in respects an enrolled authenticator (`two_factor::start_challenge`);
passkeys are unchanged. `social_login::is_form_path` puts the POST routes in the
strict `auth_origin` list and `social_login::private_response` makes every
provider response `private, no-store` with `no-referrer`. Tests use the signed
local doubles in `app/src/social_login/{google,github}/tests.rs`, never real
credentials. Run the done checks with default features, `--no-default-features
--features auth`, `--no-default-features --features google`,
`--no-default-features --features github` and `--all-features`, and regenerate
SQLx metadata with `--all-features --all-targets`.

## Two-factor authentication

`crates/auth/src/totp.rs` is pure RFC 6238 (HMAC-SHA1 through `ring`, base32,
the `otpauth://` URL; RFC vectors as unit tests). `crates/auth/src/two_factor.rs`
owns migration 0013's `totp_secrets`, `recovery_codes` and
`two_factor_challenges` tables, enrolment, the sign-in challenge, its cookie
and the QR SVG (`qrcode` with `default-features = false`; never add an image
or rendering stack for it). `app/src/two_factor.rs` serves
`/settings/two-factor` (begin, confirm, cancel, recovery_codes, disable, all
posting to that path) and `/sign-in/two-factor`; `app/src/views/two_factor.rs`
renders them and the panel on `/settings`. Both POST routes are in the strict
`auth_origin` list.

Rules: the secret is issued only after the current password is confirmed and
counts only once `confirm` has verified a code; `is_enabled` is false while
pending, so sign-in is unchanged until then. Disabling and regenerating
recovery codes require the password. The password handler must never create a
session for an enabled account: it calls `two_factor::start_challenge`, which
sets the `two_factor` cookie (256-bit token, SHA-256 stored, ten minutes,
five attempts) and redirects to the code page; only `finish_challenge` yields
the user. Codes are single-use through `last_used_step`; recovery codes are
hashed and burned on use. Passkey sign-in bypasses the challenge by design (a
passkey is already two factors); keep it that way unless the product requires
otherwise. Recovery codes are rendered once, from the response that created
them, like API keys. Never log codes, secrets or tokens. Run both workspace
checklists in every feature mode and regenerate SQLx metadata in both
workspaces when the auth crate's SQL changes.

## Background jobs

`crates/core/src/jobs/` owns the durable queue; `app/src/jobs.rs` owns this
app's job kinds and `jobs::registry()`. Migration `0010_jobs.sql` creates the
`jobs` table. The user guide is `app/content/docs/background-jobs.md`.

Choosing where work runs:

- The response needs it: do it in the handler. CPU-heavy: `blocking::run`.
- It must still happen after a crash or deploy, it talks to another system,
  and the user is not waiting on it: a job. Enqueue inside the transaction
  that creates the related data (`enqueue(&mut *tx, …)`), with a
  `dedupe_key` when the same trigger can fire twice.
- Cheap, idempotent, periodic cleanup where a missed tick costs nothing: a
  Tokio interval in `main.rs`, gated by `role.runs_worker()` like the
  existing ones. Everything else recurring is a job enqueued with `run_at`.
- Never move signup codes or other time-boxed security flows into jobs.

Rules: `KIND` strings are permanent; add payload fields with
`#[serde(default)]` and add a new kind instead of changing a payload
incompatibly. Handlers run at least once, so they must be idempotent and must
return errors rather than panic (`?` retries, `JobError::discard` gives up).
Payloads hold identifiers only. Every new kind is registered in
`jobs::registry()` and has a test in `app/tests/jobs.rs` that runs the worker
in-process. `JOBS_WORKER` (`true`/`false`/`only`) and the `worker` command
choose the process role; a worker-only process serves `worker_router()` so the
container health check passes. Keep `JOBS_SHUTDOWN_SECONDS` under the host's
stop grace period. Run the standard checks plus `cargo test --test jobs` in
every feature mode when touching the queue or a job.

## Automatic IndexNow

`crates/core/src/indexnow.rs` (under `template/` for the site) owns per-origin
ownership keys, durable snapshots and the background worker. The app calls
`indexnow_paths` for published public pages only. Keep this list aligned with
the sitemap; never add account, auth or private URLs. `app/build.rs` fingerprints
app source/assets/content so non-Docker builds also notify on changes.

The app's migration creates `indexnow_state`; keys and checkpoints survive
restarts. Failed submissions never advance checkpoints. The worker verifies
the public ownership endpoint's revision before sending, serializes replicas
with a row lock, and submits at most 10,000 URLs per request.
`INDEXNOW_ENABLED=false` disables it for staging. Local/non-HTTPS origins skip.
Run both workspace checks, template content feature combinations and the
core IndexNow database/HTTP-double tests. Never contact a real search engine in tests.

## Optional API

The `api` Cargo feature defaults on. `app/src/api.rs` mounts `/api/v1` with its
own JSON fallback; routes take the `ApiUser` extractor, which accepts only
`Authorization: Bearer` with an API key (`sr_`) or an OAuth access token
(`sro_`, see "Optional OAuth" below) and rejects with a JSON 401 whose
`WWW-Authenticate` names the resource metadata. Cookie sessions
never authenticate the API and no CORS headers are sent. Handlers return
`Result<_, ApiError>`, a JSON mirror of `AppError` that keeps error reporting
in one place. Two endpoints ship with every project: `GET /api/v1` (public
service info) and `GET /api/v1/me`. Product endpoints go in the same router;
scope their queries by the key owner's id.

The API documents itself with `utoipa` (optional dependencies of the `api`
feature). Every endpoint is registered in `api::documented_routes` with
`utoipa_axum::routes!` and carries a `#[utoipa::path]`: summary from its doc
comment, `security(("api_key" = []))` unless public, every status it returns
with a description. Bodies are `ToSchema` structs sent with `json_body`,
never `json!` literals, so the document cannot drift from the wire format.
A route added with plain `.route` is served but undocumented; don't. The
document is built once (`api::openapi`, title from `views::SITE_NAME`) and
served publicly at `/openapi.json` and `/api/v1/openapi.json`; `/api/v1/docs` renders it in
Swagger UI from jsDelivr, pinned by version and SRI hash in
`views/api_docs.rs`, with `persistAuthorization: false` so keys never reach
browser storage. The privacy page lists jsDelivr when the feature is on. The
unit test in `api.rs` lists every documented operation; extend it with
each new endpoint.

`app/src/api_keys.rs` owns the `api_keys` table (migration 0009, present in
every build): a `sr_` token with 256 random bits, stored as SHA-256 plus a
display prefix, at most ten per account, deleted with the account. Keys are
created and revoked only on the settings page; the creation response renders
the plaintext once instead of redirecting, because the token exists nowhere
else. Authentication is one statement that also records `last_used_at` at
most once a minute. Email changes do not revoke keys. The guide is
`app/content/docs/api.md`. Run the done checks with default features,
`--no-default-features`, `--no-default-features --features api` and
`--all-features`.

## Optional OAuth

The `api` feature also makes the app an OAuth 2.1 authorization server, so
hosted agents (Claude, ChatGPT, Claude Code and any MCP client following the
protocol's authorization specification) connect with a sign-in and consent
instead of a pasted key. `crates/auth/src/oauth_server.rs` owns the protocol and the
SQL (migration 0015: `oauth_clients`, `oauth_codes`, `oauth_tokens`, present
in every build): RFC 8414 and RFC 9728 metadata, RFC 7591 dynamic client
registration (open, bounded by redirect-URI validation and a 90-day purge of
clients that never issued a token), the authorization code grant with PKCE
`S256` required of every client, refresh tokens that rotate on use, RFC 8707
resource indicators, RFC 7009 revocation and the `iss` response parameter.
Tokens are opaque `sro_`/`srr_` strings stored as SHA-256 like keys; there
are no JWTs, signing keys or scopes (a token has the account's access; the
requested scope is stored and echoed). Access tokens last one hour, refresh
tokens 30 days, codes ten minutes, at most 25 live grants per account.

`app/src/oauth_server.rs` is the HTTP surface and `views/oauth_server.rs` the consent and
refusal pages. The metadata, `/oauth/register`, `/oauth/token` and
`/oauth/revoke` are mounted after the same-origin guard because clients call
them from their own servers; `/oauth/authorize` stays behind it and in
`auth_origin`, since it is a browser form. A request whose client or redirect
URI cannot be trusted renders the refusal page and never redirects; every
other failure redirects to the client with `error`. An anonymous visitor's
request is kept in the `oauth_continue` cookie (public parameters only) and
the dashboard, where every sign-in and sign-up path lands, resumes it once.
`WWW-Authenticate` on a 401 points at `/.well-known/oauth-protected-resource`
(or `/mcp` under it), which is how MCP clients find the server; keep that
header on every unauthenticated API response. Connected apps are listed and
disconnected on the settings page (`disconnect_app`); `oauth_server::cleanup_expired`
runs hourly in the worker role. Never add a flag that skips PKCE or consent.
The guides are `app/content/docs/api.md` and `app/content/docs/mcp.md`; the
crate's tests cover the protocol and `app/tests/app.rs` the browser flow. Run
the API feature checks.

## Optional file storage

The `storage` Cargo feature defaults on (`storage = ["axum/multipart"]`).
`crates/core/src/storage.rs` is the only code that talks to object storage:
`Storage::from_env` picks an S3-compatible bucket when `STORAGE_S3_BUCKET`,
`STORAGE_S3_ENDPOINT`, `STORAGE_S3_ACCESS_KEY_ID` and
`STORAGE_S3_SECRET_ACCESS_KEY` are all set (`STORAGE_S3_REGION` defaults to
`auto`; AWS needs the real region) and a local directory (`STORAGE_LOCAL_DIR`,
default `./storage`, `/app/storage` in the image) when all are blank. Partial
configuration fails startup. Requests are path-style and signed with
Signature Version 4 by hand over the `reqwest` client already in the tree;
never add an AWS SDK for this. Keys are validated (`[A-Za-z0-9._-]` segments,
no `..`) before any backend sees them. Objects are read and written whole:
uploads of a few megabytes, not streaming. `STORAGE_S3_PUBLIC_URL` is
optional; without it the app serves what it needs to. The crate's tests use
an in-process S3 double and a temp directory, never a real bucket;
`scratch_storage()` is what `build` and the tests use, `main` reads the env.

Profile pictures (`app/src/avatars.rs`; migration 0012 adds
`users.avatar_key`, present in every build) are the worked example and the
pattern for other uploads: a key prefix per kind
(`avatars/<user id>/<random>.<ext>`), the type sniffed from the bytes (PNG,
JPEG, GIF, WebP; never SVG, never the client's content type), a 2 MiB limit,
the key on the owning row, the previous object deleted after the row
changes, and a prefix-scoped public route (`GET /avatars/{*path}`, immutable
caching). `POST /settings/avatar` is multipart, so it sits outside the 64 KiB
form body limit in its own nested router with the same-origin guard and a
`DefaultBodyLimit`; failures re-render the settings page with a 422 like the
form actions, success redirects to `/settings?saved=avatar`. The shared auth
`User` carries `avatar_key` from the session lookup, so the navbar shows it
without a query. Account deletion discards the object after the transaction
commits. The guide is `app/content/docs/file-storage.md`. Run the done
checks with default features, `--no-default-features`,
`--no-default-features --features storage` and `--all-features`.

## Optional MCP server

The `mcp` Cargo feature defaults on and depends on `api` (`mcp = ["api"]`):
`app/src/mcp.rs` serves `POST /mcp` with the `ApiUser` extractor, so the
same bearer keys and OAuth tokens authenticate and cookie sessions never do;
its resource metadata is served at `/.well-known/oauth-protected-resource/mcp`. The protocol is
`app_core::mcp` (`crates/core/src/mcp.rs`): stateless Streamable HTTP,
one JSON-RPC message or 2025-03-26 batch per request, JSON responses, no
sessions or event streams, `GET` is a 405; methods `initialize`, `ping`,
`tools/list`, `tools/call`, versions `2025-06-18`, `2025-03-26`,
`2024-11-05`; everything else is `-32601`. The app implements
`app_core::mcp::Tools`: `list` returns the tool documents, `call`
returns `Ok(None)` for an unknown name, `tool_result`/`tool_error` for
outcomes the model can act on, and `Err` only for internal failures, which
become `-32603` with the source kept on the response for the error
boundary. `get_account` ships with every project and must stay equal to
`GET /api/v1/me` (`api::account_json`). Product tools scope their queries by
the key owner's id, describe themselves for a model, and mark destructive
ones with `destructiveHint`. Never return large payloads inline; hand out a
URL. Keep provider and product code in the app; the core module stays
protocol-only. The guide is `app/content/docs/mcp.md`. Run the done checks
with default features, `--no-default-features`, `--no-default-features
--features mcp` and `--all-features`.

## Admin panel

Every build includes `/admin`, guarded by `CurrentUser` and `admin::require_admin`
before reading account data. Migration 0008 adds `users.is_admin`, default false
for both existing accounts and new signups. The shared auth user carries this
flag; the existing session lookup reads it from Postgres on every request.
Role changes therefore apply to existing sessions on their next request, without
an extra query for the navbar. Only admins see Admin panel in the account dropdown.

The panel lists email, join date and role, 50 accounts per page using an indexed
UUID cursor. It is read-only, private/no-store, excluded from the sitemap and
IndexNow, and independent of optional content, telemetry and billing features.
Admin status grants complimentary paid-feature access through
`billing::require_access` when Stripe support is enabled, even without provider
configuration. It does not create billing records, bypass ownership checks,
or cancel existing subscriptions. Role revocation applies immediately to
sessions and API keys; actual purchase/subscription access remains independent. Operators assign roles to existing
accounts with `cargo run -- admin grant <email>` or `cargo run -- admin revoke <email>`.
Commands apply pending migrations using `DATABASE_URL`, normalize the email, and
fail for missing accounts. Public signup/profile forms cannot assign roles.
Future admin handlers must check the current request's user before privileged work.
Run the workspace checklist, content feature combinations, and all features
when changing user/session queries; regenerate the offline SQLx cache.

## Mutation testing

`.cargo/mutants.toml` configures `cargo-mutants` 27.1.0 with workspace-wide tests
and a clean baseline. CI checks changed Rust code and fails on survivors,
timeouts, or baseline/tool failures; reports are uploaded even on failure.
Run `bash scripts/ci/mutation-test.sh <base-sha>` from this workspace to reproduce
(including staged, unstaged and untracked Rust files),
or omit the base for a full audit. Test-only/configuration changes need a focused
or full audit because diff filtering may find no mutants. Mutation audits enable
all features so feature-gated source is exercised; ordinary CI still checks
the generated defaults. Investigate survivors with behavior tests; never hide failures
with broad exclusions or `continue-on-error`. Keep reports out of source and
generated archives. See the mutation-testing guide for triage and full audits.

## Fuzz testing

`fuzz/` is an independent cargo-fuzz workspace with its own lockfile. Keep it out
of the application's Cargo members and release dependencies. `scripts/fuzz.sh`
replays reviewed `fuzz/seeds/` inputs and runs every target (or explicit target
names after the seconds argument) under nightly
AddressSanitizer with bounded time, input size and memory. Add targets for new
untrusted-input parsers and validators; unexpected panics are failures, ordinary
validation errors are not. Minimize findings and add a seed plus a unit regression.
Run `cargo fmt --manifest-path fuzz/Cargo.toml --all --check`,
`cargo deny --manifest-path fuzz/Cargo.toml --config deny.toml check` and
`bash scripts/fuzz.sh 30` after harness changes. Never commit live credentials or
raw corpus/artifact directories. See `fuzz/README.md` for commands.

## Branch coverage

`bash scripts/coverage.sh` measures Rust branch outcomes with pinned
`nightly-2026-10-01`, `llvm-tools-preview` and `cargo-llvm-cov` 0.9.1.
Keep tool pins aligned with CI and README. Tests use a disposable Postgres
`DATABASE_URL` with CREATEDB and checked offline SQL metadata. Reports live in
`target/coverage/` (HTML, JSON, LCOV and a Markdown branch summary). Missing
branch data fails the job; there is no percentage threshold. Use uncovered
branches to guide tests, not implementation changes made just to improve a score.
Run `python3 -m unittest discover -s scripts -p 'test_coverage_*.py'` from
the directory containing the coverage summary script when changing it.

## Public HTML and Markdown

Register public marketing pages with `routes::public_page`, including new landing
pages and content indexes. Published blog and docs articles are registered from
their content collections at startup. The shared core `public_page::PublicPage`
converts anonymous HTML once using an HTML parser; requests reuse the Markdown
bytes. HTML continues to render with the current user's navigation and metadata.
Both representations include `Vary: Accept`; explicit `text/markdown` is selected
using the shared quality-aware negotiation, while browsers and wildcards get HTML.
Keep account forms, APIs, feeds, assets, redirects and errors outside this helper.
Do not pass private or user-specific HTML to the startup converter. No separate
Markdown copy or per-request conversion is needed when adding a public page.
