+++
title = "Getting started"
slug = "getting-started"
description = "Run the app and try sign-up, sign-in, and your dashboard."
section = "getting-started"
order = 1
+++

## Run the app locally

With Rust and Docker installed, open a terminal in the project folder:

```sh
docker compose up -d
cp .env.example .env
SQLX_OFFLINE=true cargo run
```

Open [localhost:3000](http://localhost:3000). Keep Docker running for the local Postgres database. The app applies its database migrations at startup. The first build uses `SQLX_OFFLINE=true` because the new database has no tables until then; afterwards plain `cargo run` checks your queries against it.

## Create an account

Configure the email settings described in [Email verification](/docs/email-verification) first. Choose [Create an account](/sign-up), enter your email address, and set a password with at least 8 characters. Enter the six-character code sent to your email in the same browser. Only successful verification creates your account, signs you in, and takes you to the dashboard.

This account belongs to this running app and its database. Accounts from another deployment are separate.

## Try sign-in and sign-out

1. Choose **Sign out** in the header to end your current session.
2. Open the [dashboard](/dashboard). You will be asked to sign in.
3. Enter your email and password on the [Sign in](/sign-in) page to return to the dashboard.

A browser cookie keeps you signed in between requests. If the session expires, sign in again. Public pages, including these docs, remain available without an account.

## Build on the account flow

The sign-up, sign-in, and sign-out handlers live in `app/src/routes.rs`; their forms live in `app/src/views/pages.rs`. The authentication crate handles password hashing and sessions.

To protect another page, use the `CurrentUser` extractor in its handler, following the dashboard example. It supplies the signed-in user and redirects visitors without a session to sign-in. Use that user's ID to scope any account-owned data you add.

Read `README.md` and `AGENTS.md` before changing the app, and update these guides as your product grows.
