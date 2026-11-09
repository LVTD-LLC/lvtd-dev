+++
title = "Sentry"
slug = "sentry"
description = "Monitor errors and performance with your own Sentry project."
section = "features"
order = 3
+++

## Enable Sentry

Sentry is optional. Your project's selected capabilities are in the default feature list in `app/Cargo.toml`, independently of [PostHog](/docs/posthog). Add or remove the five features below to change what is collected, then rebuild.

## Choose capabilities

- **Error tracking** (`sentry-errors`): browser exceptions, server errors with source chains, and panic reports.
- **Traces** (`sentry-traces`): server and browser request traces, plus supported Chromium browser profiling.
- **Application logs** (`sentry-logs`): structured Rust and browser application logs.
- **Metrics** (`sentry-metrics`): request counts, response times, and browser page-load counts.
- **Session replay** (`sentry-replay`): masked browser session recordings.

## Configure your project

Set your own Sentry DSN in `.env` locally and in your deployment environment:

```sh
SENTRY_DSN=your-project-dsn
SENTRY_ENVIRONMENT=production
SENTRY_TRACES_SAMPLE_RATE=1.0
SENTRY_REPLAY_SAMPLE_RATE=0.25
SENTRY_PROFILE_SAMPLE_RATE=0.25
```

Use `SENTRY_ENVIRONMENT=development` locally. A blank `SENTRY_DSN` disables both server and browser SDKs, including browser script loading.

## Adjust collection

All three sample rates accept values from `0` to `1`. The defaults collect all request traces, sample 25% of replay sessions and supported browser profiles, and capture replays for all error sessions when both replay and error tracking are enabled. Replay alone uses session sampling. Sampling settings apply only to enabled capabilities.

Replay masks text and inputs and blocks media. Request bodies, cookies, query strings, and user profiles are not attached. Review your own error messages and log fields before including private data. Profiling is for supported browsers; the integration does not profile Rust code.

## Change the choice later

Add or remove the five `sentry-*` capabilities listed above in the default features in `app/Cargo.toml`. Removing all five omits the integration. The `sentry`, `sentry-server`, and `sentry-layer` features are internal dependency groups; select the individual capabilities instead. Rebuild and restart after changing features or configuration. Sentry and PostHog can run together; each uses your separate provider configuration.
