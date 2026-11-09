+++
title = "Two-factor authentication"
slug = "two-factor"
description = "Let users add an authenticator app as a second step when signing in with a password."
section = "features"
order = 7
+++

## What users get

Every build ships two-factor authentication with an authenticator app
(TOTP, the six-digit codes 1Password, Google Authenticator, Authy and the
like produce). A signed-in user opens **Account settings**, confirms their
password, scans a QR code (or types the key by hand) and enters one code to
turn it on. They are shown eight single-use recovery codes once, for the day
the phone is lost.

From then on, signing in with the password lands on a code prompt instead of
the dashboard. A correct authenticator code or an unused recovery code
finishes the sign-in; five wrong codes send the user back to the password
form. Signing in with a passkey skips the prompt: a passkey already proves
possession of a device and a biometric or PIN, so it is two factors by
itself.

Users can generate new recovery codes (which replaces the old set) or turn
the feature off, both after confirming their password. Deleting the account
removes the secret and the codes.

## Configuration

There is nothing to configure. The issuer shown in the authenticator app is
the site name, and the account label is the user's email address.

## What is enforced

- Codes follow RFC 6238: HMAC-SHA1, six digits, 30-second steps, with one
  step of clock drift either way. A code is accepted at most once: the step
  it was used for is recorded and nothing at or before it is accepted again.
- The 160-bit secret is generated server side and stored as-is in
  `totp_secrets` (it must be recoverable to check codes). It is shown exactly
  once, on the enrolment page, after the password has been confirmed, and it
  only counts once a code has proven the app is set up. Until then sign-in
  is unchanged.
- Recovery codes are ten characters from an alphabet without look-alikes,
  about 50 bits each, stored as SHA-256 hashes and marked used on first use.
- Between the password and the code the browser holds a random 256-bit
  challenge token in an `HttpOnly`, `SameSite=Strict` cookie; only its hash
  is stored. A challenge lasts ten minutes, allows five attempts and is
  deleted when answered, exhausted, or when two-factor is turned off.
  There is no session until the code is right.
- Starting enrolment, regenerating recovery codes and turning the feature off
  all require the current password, like adding a passkey. The forms post to
  the page they are on and require the site's own `Origin`.
- The QR code is rendered on the server as inline SVG (the `qrcode` crate
  with no default features, so no image stack), with the four-module quiet
  zone and fixed black-on-white colours so it scans on the dark theme too.

## Code map

- `crates/auth/src/totp.rs`: the RFC 6238 arithmetic, base32 and the
  `otpauth://` URL, with the RFC test vectors as unit tests.
- `crates/auth/src/two_factor.rs`: the `totp_secrets`, `recovery_codes` and
  `two_factor_challenges` tables (migration 0013), enrolment, the sign-in
  challenge, the cookie and the QR SVG.
- `app/src/two_factor.rs`: the handlers for `/settings/two-factor` and
  `/sign-in/two-factor`; `app/src/views/two_factor.rs` the pages and the
  panel on account settings.

To require a second factor for every account, redirect users whose
`two_factor::status` is `Off` to `/settings/two-factor` from the pages you
want to protect, or check `two_factor::is_enabled` in a middleware of your own.
