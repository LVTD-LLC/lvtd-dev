+++
title = "GitHub sign-in"
slug = "github-sign-in"
description = "Let people sign up and sign in with their GitHub account, and connect GitHub to an account they already have."
section = "features"
order = 9
+++

## What users get

With the `github` Cargo feature on (the default) and a GitHub OAuth App
configured, the sign-up and sign-in pages show a **Continue with GitHub**
button below the email form. People whose primary GitHub email is verified
can create an account with one click: no password, no verification code.
Signing in again with the same GitHub account lands on the dashboard, and the
account keeps working if its GitHub email or username changes, because the
permanent numeric GitHub user id is what identifies it.

People who already have an account connect GitHub from **Account settings**
after confirming their password; from then on either method signs them in.
GitHub never joins an existing account on its own: a GitHub sign-in whose
email matches an account that is not connected is told to sign in the usual
way and connect GitHub in settings. Disconnecting GitHub asks for the password
and signs out every other browser.

Accounts created with GitHub have no password. Instead of a password check,
sensitive actions (changing the email address, adding a passkey, managing
two-factor authentication, deleting the account) ask them to **Confirm with
GitHub** first; a confirmation on the same browser session lasts five minutes.
They can connect [Google](/docs/google-sign-in) as a second provider and then
disconnect either one, but never their last. An enrolled authenticator app
still applies: GitHub sign-in ends on the code prompt, never directly in a
session.

## Configuration

1. On GitHub, create an **OAuth App** (not a GitHub App): your organisation's
   or your own Settings → Developer settings → OAuth Apps → New OAuth App.
2. Set the homepage URL to `SITE_URL` and the authorization callback URL to
   exactly `SITE_URL` + `/auth/github/callback`, for example
   `https://app.example.com/auth/github/callback`. An OAuth App has one
   callback URL, so create a second app for local development with
   `http://localhost:3000/auth/github/callback`. Leave Device Flow off.
3. Generate a client secret and put both values in the environment:

   ```
   GITHUB_CLIENT_ID=Ov23li...
   GITHUB_CLIENT_SECRET=...
   ```

Both blank hides the button and every GitHub route answers with a redirect
back to the sign-in page. Setting one without the other stops the app from
starting, so a half-configured deployment fails loudly. `SITE_URL` must be
HTTPS or `http://localhost`.

## What is enforced

- Only the `user:email` scope is requested, with S256 PKCE and
  `prompt=select_account`. After the code exchange the server reads
  `GET /user` (for the numeric id) and `GET /user/emails`, then revokes the
  access token, so no GitHub credential outlives the request. Revocation is
  best effort and never affects sign-in.
- Only a verified, primary, deliverable address (not a
  `users.noreply.github.com` alias) may create an account. Others sign up with
  the email code and connect GitHub afterwards.
- Flows, linking, confirmations and the rest of the rules are shared with
  Google sign-in: random `state` and browser token stored only as hashes,
  single-use ten-minute callbacks, re-checks after the round trip under a row
  lock, same-origin `POST` forms, and `private, no-store` responses with a
  `no-referrer` policy. Provider responses are size-bounded and never enter
  logs or error messages.

## Code map

- `app/src/social_login/github.rs`: the GitHub client, the authorization URL,
  the code exchange, the two API reads and token revocation.
- `app/src/social_login.rs`: the provider-neutral routes (`POST /auth/github`,
  `GET /auth/github/callback`, `POST /settings/github`), account creation,
  linking, disconnecting and the session-bound confirmation check.
- `migrations/0016_social_login.sql`: `users.passwordless`,
  `social_identities`, `social_login_flows` and `social_login_confirmations`
  (present in every build, so providers can be turned on later).

To add another provider, implement `social_login::Client` for it, add a Cargo
feature that turns on `social-login`, list it in `social_login::SUPPORTED`,
read its configuration in `Providers::from_env` and give it a button icon in
`views::pages`. No migration is needed.
