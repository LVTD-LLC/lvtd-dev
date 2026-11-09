+++
title = "Stripe billing"
slug = "stripe"
description = "Configure hosted Checkout, subscriptions, one-time payments, and the customer portal."
section = "features"
order = 50
+++

## Hosted billing

The optional `stripe` Cargo feature adds a signed-in `/billing` page, Stripe-hosted
Checkout, customer portal, and a signed webhook at `/stripe/webhook`. Enable it in
`app/Cargo.toml` or run `cargo run --features stripe`. With the feature disabled,
billing routes and Stripe dependencies are omitted; source and migrations remain
available. Leaving every Stripe setting blank keeps the app runnable, with billing
unavailable. Partial configuration fails startup.

## Configure your account

Start in a separate [Stripe sandbox](https://docs.stripe.com/sandboxes). Create a
product and recurring price for subscriptions; optionally create a one-time price.
This starter supports one price per mode and quantity one. Prices are selected on
the server and validated at startup for their type, activity, and test/live mode.

Set these values in your ignored `.env` file or your host's environment:

```dotenv
STRIPE_SECRET_KEY=rk_test_your_restricted_key
STRIPE_WEBHOOK_SECRET=whsec_your_endpoint_secret
STRIPE_SUBSCRIPTION_PRICE_ID=price_your_recurring_price
STRIPE_PAYMENT_PRICE_ID=price_your_one_time_price
SITE_URL=http://localhost:3000
```

Configure either price or both. Use the same account and test/live mode for all
values. Prefer a restricted key with Checkout Sessions write/read, Prices read,
Subscriptions read, and Customer Portal write permissions (plus any dependent
permissions Stripe requests). Keep keys and webhook secrets on the server.
No publishable key or Stripe.js is needed: the browser follows a server-created
Checkout URL. Payment methods come from your Stripe Dashboard settings.

Enable the [customer portal](https://docs.stripe.com/customer-management) in the
same environment. Allow payment-method updates, invoice access and cancellation.
Plan changes, quantities and multiple products require extending the starter's
single-price validation; leave portal plan switching off until then.

## Products, tiers, and billing intervals

The starter currently accepts one recurring price through
`STRIPE_SUBSCRIPTION_PRICE_ID` and one optional one-time price through
`STRIPE_PAYMENT_PRICE_ID`. It does not yet include a plan catalog, tier selection,
or a monthly/yearly selector. Products and prices are created in your Stripe
account; the app uses their price IDs rather than creating products itself.

For three tiers, such as Starter, Pro, and Business, create **three Stripe
products with two recurring prices each**: one monthly and one yearly. That is
six prices total. The product identifies the tier; the price determines the
amount and billing interval. See [Stripe's product and price model](https://docs.stripe.com/products-prices/how-products-and-prices-work#model-your-product-catalog).

Supporting this in the app requires extending the starter:

1. Replace the single recurring-price setting with a small server-side catalog
   mapping each tier and billing interval to an allowed Stripe price ID.
2. Add tier and interval selection to `/billing`. Resolve those choices through
   the catalog on the server before creating Checkout.
3. Store the subscription's current tier and price so product routes can apply
   the appropriate feature limits.
4. Update webhook validation to accept the configured prices and update the
   stored tier when the subscription changes. Then configure the customer portal
   for the plan changes you support and test upgrades and downgrades.

The hosted Checkout flow, signature verification, durable checkout attempts, and
most lifecycle handling can remain. Creating six prices in Stripe alone does not
add tier support: the current webhook checks the subscription against its original
checkout price. Switching to another price currently removes subscription access,
so keep portal plan switching disabled until that validation is extended.

A simple `Free / Starter / Pro / Business` plan model is a useful extension.
Monthly and yearly prices for the same tier should normally grant the same
features; the interval changes billing, not permissions. This model is a suggested
customization, not functionality already included in the starter.

## Configure webhooks

Register `https://your-domain/stripe/webhook` for your account, using API version
`2026-08-26.dahlia`, and copy that endpoint's signing secret. Subscribe to:

- `checkout.session.completed`
- `checkout.session.async_payment_succeeded`
- `checkout.session.async_payment_failed`
- `checkout.session.expired`
- `customer.subscription.created`
- `customer.subscription.updated`
- `customer.subscription.deleted`
- `customer.subscription.paused`
- `customer.subscription.resumed`
- `invoice.paid`
- `invoice.payment_failed`
- `invoice.payment_action_required`

For local testing, forward events with the Stripe CLI:

```sh
stripe listen --forward-to localhost:3000/stripe/webhook
```

Use the `whsec_…` printed by that listener locally. Sign up and use the actual
`/billing` buttons in your sandbox. Generic CLI fixture events don't carry the
application's checkout metadata and will not grant access. Test successful and
declined payments, delayed methods, renewal failure/recovery, cancellation at
period end, and immediate cancellation. Use Stripe test clocks for renewals.

The handler verifies the raw-body HMAC and a five-minute timestamp tolerance,
then retrieves current Stripe objects. Account locks and canonical retrieval
make duplicate and out-of-order deliveries safe. Transient failures return 5xx
so Stripe can retry. The success URL never grants access. Pending delayed
payments stay pending until Stripe confirms them.

## Use billing in your product

`app/src/billing.rs` owns billing configuration, Checkout, database queries and
webhooks. Keep product-specific changes there.

### Free and paid users

Billing state is stored separately from the user record in `billing_accounts`
and `billing_subscriptions`. The starter tracks whether a user has paid access,
but it does not automatically restrict product features. Signup, sign-in, and
the dashboard are available to free and paid users alike; `/billing` reflects
their billing status. No shipped product route calls the paid-access helper yet.

To protect a route you add, extract `CurrentUser` and call:

```rust,ignore
billing::require_access(&state.pool, user.id).await?;
```

The helper returns HTTP 402 (Payment Required) for users without access. The
initial access policy recognizes:

- An active admin role: complimentary access to every feature using this gate, even when Stripe credentials are not configured.
- A confirmed one-time purchase: permanent access.
- A trialing subscription: access for the trial.
- An active subscription whose latest invoice is paid: subscription access.

An active subscription with an unsettled asynchronous payment does not grant
access. Past-due, unpaid, incomplete, paused, and canceled subscriptions have no
subscription access. Scheduled cancellation keeps access until Stripe changes
the subscription status at period end. A confirmed one-time purchase continues
to grant access independently of subscription status.

Admin access reads the current role from the database; existing sessions and API
keys lose the bypass as soon as the role is revoked. Actual purchase or active
subscription access survives revocation. Use `BillingStatus::has_access()` for
feature decisions; `paid` remains a truthful one-time-purchase flag, not an
entitlement check. API product handlers can call the same `require_access` helper
after extracting `ApiUser`; always scope resource queries by the authenticated
user ID. Admin access does not bypass ownership checks.

Admins are not offered new checkout sessions. Existing billing records and
subscriptions are unchanged, and the customer portal remains available for
existing customers. Granting admin access does **not** cancel recurring charges;
cancel an existing subscription explicitly through the portal when desired.
Projects with the Stripe feature disabled have no built-in paid-feature gates.

For conditional UI, read `billing::status(&state.pool, user.id).await?` and use
`BillingStatus::has_access()`. Do not use `BillingStatus.paid` as the general
paid/free check: that field only represents a one-time purchase;
`has_access()` also recognizes subscription and admin access. Hiding a button does not
protect a route, so enforce access in its server handler too.

The helper currently answers whether the user has any paid access, not which
tier they have. Extend the plan model and server-side checks to enforce tier
features or limits. Customize the policy for grace periods or per-product
entitlements. Billing state is local; ordinary protected requests do not call
Stripe.

### Checkout recovery and operational boundaries

Repeated clicks reuse an existing Checkout. If its creation response is lost,
retries reuse the same idempotency key. Unknown outcomes older than 23 hours stop
retrying: locate the Session in Stripe by `metadata.billing_checkout_id`, then
resend its webhook or reconcile the `billing_checkouts` row before trying again.
Do not blindly clear an unresolved attempt: it could already have charged.

This starter does not automate taxes, prorations, refunds, disputes, email
notifications, or fulfillment beyond access. Enable Stripe's payment-failure
emails and review Dashboard notifications for refunds/disputes; add handlers and
an explicit access policy before automating them. Refunding a one-time purchase
does not clear `billing_accounts.paid_at`, and refunding an invoice does not cancel
a subscription. Preserve checkout metadata and old price records when evolving
billing. Monitor webhook delivery failures and replay failed events after outages.

The local test suite uses signed events, a fake Stripe HTTP server, and real
Postgres. It never contacts Stripe or charges a card:

```sh
cargo test --workspace --features stripe
cargo test --workspace --no-default-features --features stripe
```
