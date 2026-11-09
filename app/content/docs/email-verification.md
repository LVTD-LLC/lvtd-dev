+++
title = "Email verification"
slug = "email-verification"
description = "Codes go to your log locally and through Mailgun in production."
section = "features"
order = 5
+++

## Sign up locally

Signup requires email verification in every build, and it works without any
setup on your machine. With no mail provider configured, the app prints every
email it would send, verification code included, to its log:

```
WARN lvtd_dev::verification: Console email (not sent)
To: you@example.com
Subject: Verify your LVTD email

Your LVTD verification code is: 7KQ2MN
```

Copy the code from the terminal running `cargo run` into the browser. Console
mode only starts on a non-HTTPS `SITE_URL`: a deployment with the Mailgun
values missing fails to start instead of writing codes into production logs.

## Configure email delivery

Before deploying, configure your own Mailgun US sending domain and its DNS
records, then set these runtime variables in `.env` locally or your hosting
provider's secret settings:

- `MAILGUN_SENDING_KEY`: a domain-scoped sending key, not an account admin key.
- `MAILGUN_DOMAIN`: your Mailgun sending domain, such as `mg.example.com`.
- `MAIL_FROM`: an address on that domain, such as `hello@mg.example.com`.
- `EMAIL_VERIFICATION_KEY`: an independent random secret; generate it with
  `openssl rand -hex 32`. Console mode uses a fixed development key when it
  is blank.

Never commit these values or put them in generation answers. Restart the app
after configuration changes. The three Mailgun values blank means console
mode; a partial configuration prevents startup. Delivery uses the Mailgun US
API with an eight-second timeout.

## Complete signup

Enter your email and password, then enter the emailed six-character code in
the same browser. No user or session exists until the code is accepted. Check
your spam folder if the email does not arrive. Codes expire in ten minutes and
allow five attempts. Request another code after one minute; only the newest
code works. If you used the wrong address or changed browsers, start again.

A failed send leaves signup pending and offers a resend after the cooldown.
There is no automatic retry after an ambiguous provider response. Signups
using an existing account's address never overwrite or authenticate that account.

## Operate the flow

The app stores only the password hash, keyed code hash, and hashed browser
token. A separate HttpOnly, SameSite=Strict cookie binds the code to the
browser; HTTPS enables Secure cookies. Each address has a shared signup/resend
budget of five requests per hour, with a global ceiling of 200 per hour.
These limits are defined in `app/src/verification.rs`. Pending records and
rate buckets older than a day are cleaned up hourly.

The migration is additive and preserves existing accounts. Existing accounts
are not retroactively marked verified. Email changes in settings require the
current password but do not yet verify the replacement address.

For local automated checks, run `cargo test --workspace` and
`node --test app/tests/*.test.cjs`. The tests capture emails with local HTTP
doubles; no Mailgun credentials or real recipients are required.
