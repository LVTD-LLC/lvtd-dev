+++
title = "PostHog"
slug = "posthog"
description = "Choose web analytics, product events, errors, logs, and request traces."
section = "features"
order = 4
+++

## Enable PostHog

PostHog is optional. Your project's selected capabilities are in the default feature list in `app/Cargo.toml`. Add or remove the four features below to change what is collected, then rebuild. PostHog works independently of [Sentry](/docs/sentry).

Configuration examples below use placeholders; provide your own ingestion token to activate collection.

## Choose capabilities

- **Web analytics** (`posthog-web`): browser pageviews, pageleaves, and Core Web Vitals.
- **Product events** (`posthog-product`): successful sign-up, sign-in, and sign-out events, plus user profiles with UUID, email, and account creation date.
- **Application logs** (`posthog-logs`): batched structured Rust logs.
- **Request traces** (`posthog-traces`): sampled HTTP request spans with trace context propagation.

## Configure your project

Set your own project's public ingestion token in `.env` locally and in your deployment environment:

```sh
POSTHOG_PROJECT_TOKEN=your-project-ingestion-token
POSTHOG_HOST=https://us.i.posthog.com
POSTHOG_TRACE_SAMPLE_RATIO=0.1
```

For an EU project, use `https://eu.i.posthog.com`. Use an ingestion token, not a personal API key. A blank token disables all PostHog exports and browser SDK loading.

`POSTHOG_TRACE_SAMPLE_RATIO` accepts `0` through `1` and defaults to `0.1` for new request traces. Valid upstream sampling decisions are respected. Logs and errors are independent of trace sampling.

## Know what is collected

Browser analytics loads asynchronously, with no JavaScript build step. Replay, autocapture, heatmaps, surveys, and feature flags are disabled. Browser and server identity use the account UUID; product events intentionally include account email.

Server telemetry omits passwords, cookies, request bodies, query strings, and raw unmatched paths. Your custom error messages and log fields may still contain application data. Health checks and static assets are excluded from request telemetry.

## Change the choice later

Add or remove the four `posthog-*` capabilities listed above in the default features in `app/Cargo.toml`, then rebuild. Removing all four omits the PostHog integration. To try additional capabilities locally:

```sh
cargo run --features posthog-web,posthog-product
```

This adds capabilities to the existing defaults. See the generated README for custom event helpers and local telemetry tests.
