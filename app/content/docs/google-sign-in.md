+++
title = "Google sign-in"
slug = "google-sign-in"
description = "Let people sign up and sign in with their Google account, and connect Google to an account they already have."
section = "features"
order = 8
+++

## What users get

With the `google` Cargo feature on (the default) and a Google client
configured, the sign-up and sign-in pages show a **Continue with Google**
button below the email form. People with a Gmail or Google Workspace address
can create an account with one click: no password, no verification code.
Signing in again with the same Google account lands on the dashboard, and the
account keeps working if its Google email changes, because the permanent
Google account identifier is what identifies it.

People who already have an account connect Google from **Account settings**
after confirming their password; from then on either method signs them in.
Google never joins an existing account on its own: a Google sign-in whose
email matches an account that is not connected is told to sign in the usual
way and connect Google in settings. Disconnecting Google asks for the password
and signs out every other browser.

Accounts created with Google have no password. Instead of a password check,
sensitive actions (changing the email address, adding a passkey, managing
two-factor authentication, deleting the account) ask them to **Confirm with
Google** first; a confirmation on the same browser session lasts five minutes.
They can connect [GitHub](/docs/github-sign-in) as a second provider and then
disconnect either one, but never their last. An enrolled authenticator app still applies: Google sign-in ends on the code
prompt, never directly in a session. Passkeys keep their existing behaviour.

## Configuration

1. In [Google Cloud](https://console.cloud.google.com/auth/overview), create a
   project (or pick one), configure the consent screen with your product name,
   your domain and the `/privacy` and `/terms` URLs of your site, and create a
   **Web application** OAuth client.
2. Add the exact redirect URI `SITE_URL` + `/auth/google/callback`, for example
   `https://app.example.com/auth/google/callback`. For local development use
   `http://localhost:3000/auth/google/callback`; a separate client for
   development is a good idea. No JavaScript origins are needed.
3. Put the client ID and secret in the environment:

   ```
   GOOGLE_CLIENT_ID=1234567890-abc.apps.googleusercontent.com
   GOOGLE_CLIENT_SECRET=GOCSPX-...
   ```

Both blank hides the button and every Google route answers with a redirect
back to the sign-in page. Setting one without the other stops the app from
starting, so a half-configured deployment fails loudly. `SITE_URL` must be
HTTPS or `http://localhost`.

The "Continue with Google" button sits below the email form and uses Google's
standard "G" mark and wording from the
[sign-in branding guidelines](https://developers.google.com/identity/branding-guidelines);
keep the logo colors and label as they are if you want to stay within those
guidelines.

## What is enforced

- Only `openid email` is requested. The app never receives or stores Google
  access or refresh tokens, profile pictures or contacts.
- Every flow starts with a random 256-bit `state` and an independent 256-bit
  browser token in an `HttpOnly` cookie scoped to the callback path. The
  database stores only their hashes, plus the nonce and PKCE verifier (S256).
  A callback must present both, is consumed before the code is exchanged, and
  expires after ten minutes.
- The ID token is verified against Google's published signing keys (RS256
  only, cached with the advertised max-age, refreshed at most every 30
  seconds): issuer, audience, authorized party, expiry, issue time within ten
  minutes, and the nonce. Provider responses are size-bounded and never enter
  logs or error messages.
- Only a verified `@gmail.com` address or a Google Workspace account (the `hd`
  claim) may create an account. Google is not authoritative for other
  domains, even when it reports the email as verified; those people sign up
  with the email code and connect Google afterwards.
- A passwordless account stores an Argon2id hash of a discarded random
  password, so the shared `users` schema is unchanged and no password can
  ever match. Confirmations are bound to one session row and die with it.
- Linking and confirming are bound to the signed-in user and session that
  started them, re-checked after the round trip while the user row is locked.
  Disconnecting deletes pending flows and two-factor challenges, and a login
  racing with a disconnect is rolled back before any cookie is set.
- `POST /auth/google` and `POST /settings/google` require the site's own
  `Origin`, like the other account forms. Google responses are
  `private, no-store` with a `no-referrer` policy, so authorization codes
  never leak through caches or referrers.

## Code map

- `app/src/social_login.rs`: the provider-neutral routes (`POST /auth/google`,
  `GET /auth/google/callback`, `POST /settings/google`), account creation,
  linking, disconnecting and the session-bound confirmation check. GitHub
  sign-in shares it.
- `app/src/social_login/google.rs`: the authorization URL, the code exchange
  and ID-token verification with the cached key set.
- `migrations/0016_social_login.sql`: `users.passwordless`,
  `social_identities`, `social_login_flows` and `social_login_confirmations`
  (present in every build, so the feature can be turned on later). It replaced
  the Google-only tables of `migrations/0014_google_auth.sql` and kept their
  identities.
- `app/src/settings.rs`: `prove_owner`, the one place that decides between a
  password check and a provider confirmation for sensitive actions.

To require Google for everyone, hide the email form in
`views::pages::auth` and the sign-up route; to allow any verified email to
create an account through Google, relax `authoritative_email` in
`app/src/social_login/google.rs` and accept that Google then vouches for
addresses it does not control.
