+++
title = "Welcome to LVTD"
slug = "welcome"
description = "Your starting point for a web app built with Rust."
section = "getting-started"
order = 0
+++

## What is LVTD?

LVTD gives you a working Rust web app to build on. It includes server-rendered pages, a Postgres database, and email-and-password accounts with a dashboard for signed-in users.

Axum handles requests, Maud renders HTML, and sqlx checks database queries. The app runs as one server binary, with no JavaScript build step.

## Make it your own

Your product code lives in `app/`. Start by changing the homepage and dashboard, then add the features your users need. The `crates/` directory contains the shared core and authentication code.

These Markdown guides are a starting point too. Replace the introduction with a description of your product and keep the steps that help your users.

## Start here

[Get started](/docs/getting-started) to run the app and try its account flow. When you are ready to update these pages, follow [Writing documentation](/docs/writing-docs).

## Configure shared features

Learn how to publish a [blog](/docs/blog), customize [documentation](/docs/writing-docs), configure [Sentry](/docs/sentry) or [PostHog](/docs/posthog), accept payments with [Stripe](/docs/stripe), keep user uploads in a bucket or on disk with [file storage](/docs/file-storage), and add [vector search with Qdrant](/docs/qdrant). These guides cover optional capabilities; check `app/Cargo.toml` to see which are enabled in your project.

When you are ready to deploy, follow [Docker hosting](/docs/docker), or [CapRover](/docs/caprover) if your project includes its workflow.
