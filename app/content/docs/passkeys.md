+++
title = "Passkeys"
slug = "passkeys"
description = "Let verified users sign in with Face ID, Touch ID, Windows Hello or a security key."
section = "features"
order = 6
+++

## What users get

Every build ships passkey sign-in. A signed-in user opens **Account settings**,
confirms their current password, and the browser creates a passkey on their
device or password manager. From then on the sign-in page offers **Sign in
with a passkey**, and browsers that support it also list the passkey in the
email field's autofill. Passkeys are bound to your site's origin, so they
cannot be phished or reused elsewhere, and only the public key is stored.

Passkeys never create accounts. Signup still requires the emailed code, and a
passkey can only be attached by an account holder who is signed in and has
confirmed their password. Each account can hold ten passkeys. Removing a
passkey is a plain form in settings; deleting the account removes them all.

## Configuration

There is nothing to configure. The relying-party id is the host of `SITE_URL`
and the browser must report exactly that origin, so set `SITE_URL` to the
address users visit (`https://app.example.com`, no trailing slash). Passkeys
work on `http://localhost:3000` during development. Do not serve the app from
a different host or port than `SITE_URL`: the browser will refuse, and the
server would reject the response anyway.

## What is enforced

- Registration requests `attestation: "none"`, discoverable credentials
  (`residentKey: "required"`) and `userVerification: "required"`; the server
  checks the user-present and user-verified flags on every ceremony.
- Offered algorithms are ES256, RS256 and EdDSA (Ed25519), which cover platform
  authenticators and security keys. Signatures are verified with `ring`,
  already in the dependency tree through rustls; there is no OpenSSL.
- Challenges are 32 random bytes stored in Postgres, bound to the signed-in
  user for registration, anonymous for sign-in, single use and valid for ten
  minutes. Expired challenges are swept whenever a new one is issued.
- The client data's type, challenge and origin, the relying-party id hash and
  the signature counter are all checked; a counter that goes backwards is
  treated as a possible clone and the assertion is rejected.
- Credential ids are unique across all accounts, and a sign-in response's user
  handle must match the account the passkey belongs to.
- The JSON endpoints under `/passkeys/` require the site's own `Origin`, the
  same rule the signup and verification forms follow.

## Code map

- `crates/auth/src/webauthn.rs`: pure protocol verification, including a small
  CBOR decoder and COSE key parsing, with unit tests and a static RS256 vector.
- `crates/auth/src/passkeys.rs`: the `passkeys` and `passkey_challenges`
  tables (migration 0011), options generation and the two ceremonies.
- `app/src/passkeys.rs`: the four JSON handlers; `app/assets/passkeys.js`
  drives `navigator.credentials` on the sign-in and settings pages.
- `crates/auth/src/testing.rs` (feature `test-support`, dev-dependency only):
  a software authenticator so integration tests run the real ceremonies
  without a browser.

To relax user verification to `preferred` for security keys without a PIN,
change the option in `crates/auth/src/passkeys.rs` and drop the user-verified
check in `crates/auth/src/webauthn.rs`; keep in mind the passkey then becomes
a possession-only factor.
