//! Optional hosted billing. Customize the prices and access policy for your product.
//! Verified webhooks grant purchased access; current admins have complimentary access.
//! The browser return page is read-only.
use crate::AppError;
use anyhow::{Context, ensure};
use app_auth::User;
use axum::http::StatusCode;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use sqlx::{PgPool, Postgres, Transaction};
use std::{collections::HashMap, time::Duration};
use time::OffsetDateTime;
use uuid::Uuid;

const API_VERSION: &str = "2026-08-26.dahlia";
const CHECKOUT_TAG: &str = "billing_checkout_id";

// No Debug: credentials must not appear in logs.
#[derive(Clone)]
pub struct Config {
    secret_key: String,
    webhook_secret: String,
    subscription_price: String,
    payment_price: String,
    livemode: bool,
}
impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let read = |name| std::env::var(name).unwrap_or_default().trim().to_owned();
        Self::parse(
            read("STRIPE_SECRET_KEY"),
            read("STRIPE_WEBHOOK_SECRET"),
            read("STRIPE_SUBSCRIPTION_PRICE_ID"),
            read("STRIPE_PAYMENT_PRICE_ID"),
        )
    }
    pub fn parse(
        secret_key: String,
        webhook_secret: String,
        subscription_price: String,
        payment_price: String,
    ) -> anyhow::Result<Option<Self>> {
        if [&secret_key, &webhook_secret, &subscription_price, &payment_price].iter().all(|v| v.is_empty()) {
            return Ok(None);
        }
        let livemode = secret_key.starts_with("sk_live_") || secret_key.starts_with("rk_live_");
        ensure!(
            livemode || secret_key.starts_with("sk_test_") || secret_key.starts_with("rk_test_"),
            "STRIPE_SECRET_KEY must be a restricted or secret key"
        );
        ensure!(webhook_secret.starts_with("whsec_") && webhook_secret.len() > 6, "STRIPE_WEBHOOK_SECRET is required");
        ensure!(!subscription_price.is_empty() || !payment_price.is_empty(), "Configure at least one Stripe price");
        for price in [&subscription_price, &payment_price] {
            ensure!(price.is_empty() || valid_id(price, "price_"), "Invalid Stripe price ID");
        }
        Ok(Some(Self { secret_key, webhook_secret, subscription_price, payment_price, livemode }))
    }
    pub fn subscriptions_enabled(&self) -> bool {
        !self.subscription_price.is_empty()
    }
    pub fn payments_enabled(&self) -> bool {
        !self.payment_price.is_empty()
    }
    fn price(&self, mode: &str) -> Option<&str> {
        match mode {
            "subscription" if self.subscriptions_enabled() => Some(&self.subscription_price),
            "payment" if self.payments_enabled() => Some(&self.payment_price),
            _ => None,
        }
    }
}
fn valid_id(id: &str, prefix: &str) -> bool {
    id.starts_with(prefix) && id.len() > prefix.len() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

#[derive(Clone)]
pub struct StripeClient {
    pub cfg: Config,
    http: reqwest::Client,
    base: String,
}
#[derive(Deserialize)]
struct Price {
    id: String,
    active: bool,
    livemode: bool,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Items {
    has_more: bool,
    data: Vec<Item>,
}
#[derive(Deserialize)]
struct Item {
    quantity: Option<i64>,
    price: Price,
}
#[derive(Deserialize)]
struct Session {
    id: String,
    url: Option<String>,
    status: String,
    mode: String,
    payment_status: String,
    livemode: bool,
    client_reference_id: Option<String>,
    customer: Option<String>,
    subscription: Option<String>,
    #[serde(default)]
    metadata: HashMap<String, String>,
    line_items: Option<Items>,
}
#[derive(Deserialize)]
struct Subscription {
    id: String,
    customer: String,
    status: String,
    livemode: bool,
    cancel_at_period_end: bool,
    latest_invoice: Option<Invoice>,
    #[serde(default)]
    metadata: HashMap<String, String>,
    items: Items,
}
#[derive(Deserialize)]
struct Invoice {
    status: Option<String>,
}

impl StripeClient {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        Ok(Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: "https://api.stripe.com".into(),
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.cfg.secret_key)
            .header("Stripe-Version", API_VERSION)
    }
    async fn decode<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> anyhow::Result<T> {
        // Never log Stripe response bodies or bearer checkout/portal URLs.
        ensure!(response.status().is_success(), "Stripe API returned HTTP {}", response.status());
        Ok(response.json().await?)
    }
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        Self::decode(self.request(reqwest::Method::GET, path).send().await?).await
    }
    /// Validate each configured price once, before serving requests.
    pub async fn validate(&self) -> anyhow::Result<()> {
        for (mode, kind) in [("subscription", "recurring"), ("payment", "one_time")] {
            if let Some(id) = self.cfg.price(mode) {
                let price: Price = self.get(&format!("/v1/prices/{id}")).await?;
                ensure!(
                    price.id == id && price.active && price.kind == kind && price.livemode == self.cfg.livemode,
                    "Stripe price type, activity or test/live mode is incorrect"
                );
            }
        }
        Ok(())
    }
    async fn session(&self, id: &str) -> anyhow::Result<Session> {
        ensure!(valid_id(id, "cs_"), "Invalid Checkout Session ID");
        Self::decode(
            self.request(reqwest::Method::GET, &format!("/v1/checkout/sessions/{id}"))
                .query(&[("expand[]", "line_items")])
                .send()
                .await?,
        )
        .await
    }
    async fn subscription(&self, id: &str) -> anyhow::Result<Subscription> {
        ensure!(valid_id(id, "sub_"), "Invalid subscription ID");
        Self::decode(
            self.request(reqwest::Method::GET, &format!("/v1/subscriptions/{id}"))
                .query(&[("expand[]", "latest_invoice")])
                .send()
                .await?,
        )
        .await
    }
    async fn create(
        &self,
        user: &User,
        attempt: &Attempt,
        customer: Option<&str>,
        site: &str,
    ) -> anyhow::Result<Session> {
        let site = site.trim_end_matches('/');
        let mut form = vec![
            ("mode", attempt.mode.clone()),
            ("line_items[0][price]", attempt.price_id.clone()),
            ("line_items[0][quantity]", "1".into()),
            ("client_reference_id", user.id.to_string()),
            ("metadata[billing_checkout_id]", attempt.id.to_string()),
            ("success_url", format!("{site}/billing/success")),
            ("cancel_url", format!("{site}/billing")),
            ("integration_identifier", "hosted_billing_xqpmvrtn".into()),
        ];
        if let Some(customer) = customer {
            form.push(("customer", customer.into()));
        } else {
            form.push(("customer_email", attempt.email.clone()));
            if attempt.mode == "payment" {
                form.push(("customer_creation", "always".into()));
            }
        }
        if attempt.mode == "subscription" {
            form.push(("subscription_data[metadata][billing_checkout_id]", attempt.id.to_string()));
        }
        Self::decode(
            self.request(reqwest::Method::POST, "/v1/checkout/sessions")
                .header("Idempotency-Key", format!("billing-checkout-{}", attempt.id))
                .form(&form)
                .send()
                .await?,
        )
        .await
    }
    pub async fn portal(&self, pool: &PgPool, user_id: Uuid, site: &str) -> anyhow::Result<String> {
        let account = sqlx::query!("SELECT customer_id FROM billing_accounts WHERE user_id = $1", user_id)
            .fetch_optional(pool)
            .await?;
        let customer = account.and_then(|a| a.customer_id).context("No billing customer yet")?;
        #[derive(Deserialize)]
        struct Portal {
            url: String,
        }
        let portal: Portal = Self::decode(
            self.request(reqwest::Method::POST, "/v1/billing_portal/sessions")
                .form(&[("customer", customer), ("return_url", format!("{}/billing", site.trim_end_matches('/')))])
                .send()
                .await?,
        )
        .await?;
        hosted_url(&portal.url, "billing.stripe.com")?;
        Ok(portal.url)
    }
}
fn hosted_url(url: &str, host: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(url)?;
    ensure!(url.scheme() == "https" && url.host_str() == Some(host), "Unexpected Stripe hosted URL");
    Ok(())
}

pub struct BillingStatus {
    /// Live role, separate from actual purchases and subscriptions.
    pub is_admin: bool,
    pub paid: bool,
    pub subscription_access: bool,
    pub subscription_status: Option<String>,
    pub cancel_at_period_end: bool,
    pub has_customer: bool,
}
impl BillingStatus {
    /// Starter policy: admin access, a permanent purchase, a trial, or a paid active subscription.
    /// Past-due, unpaid, paused and canceled subscriptions do not grant access.
    pub fn has_access(&self) -> bool {
        self.is_admin || self.paid || self.subscription_access
    }
}
pub async fn status(pool: &PgPool, user_id: Uuid) -> Result<BillingStatus, sqlx::Error> {
    let account = sqlx::query!("SELECT u.is_admin, b.paid_at, b.customer_id FROM users u LEFT JOIN billing_accounts b ON b.user_id = u.id WHERE u.id = $1", user_id)
        .fetch_optional(pool)
        .await?;
    let sub = sqlx::query!("SELECT status, access_active, cancel_at_period_end FROM billing_subscriptions WHERE user_id = $1 ORDER BY (status NOT IN ('canceled', 'incomplete_expired')) DESC, updated_at DESC, id DESC LIMIT 1", user_id).fetch_optional(pool).await?;
    Ok(BillingStatus {
        is_admin: account.as_ref().is_some_and(|a| a.is_admin),
        subscription_access: sub.as_ref().is_some_and(|s| s.access_active),
        paid: account.as_ref().is_some_and(|a| a.paid_at.is_some()),
        has_customer: account.is_some_and(|a| a.customer_id.is_some()),
        cancel_at_period_end: sub.as_ref().is_some_and(|s| s.cancel_at_period_end),
        subscription_status: sub.map(|s| s.status),
    })
}
/// Use this in paid product routes after extracting CurrentUser. Auth itself stays free.
pub async fn require_access(pool: &PgPool, user_id: Uuid) -> Result<(), AppError> {
    if status(pool, user_id).await?.has_access() {
        return Ok(());
    }
    Err(AppError::Status(StatusCode::PAYMENT_REQUIRED, "Visit /billing to choose a plan.".into()))
}
struct Attempt {
    email: String,
    id: Uuid,
    mode: String,
    price_id: String,
}
pub enum Checkout {
    Redirect(String),
    Pending,
    Retry,
    AlreadyPurchased,
    AdminAccess,
}

/// Durable attempts + a per-account lock + Stripe idempotency cover repeated clicks
/// and a lost create response. An ambiguous outcome older than 23h needs reconciliation.
pub async fn checkout(
    pool: &PgPool,
    stripe: &StripeClient,
    user: &User,
    mode: &str,
    site: &str,
) -> anyhow::Result<Checkout> {
    // Do not trust a caller's potentially stale User or create billing rows for admins.
    if status(pool, user.id).await?.is_admin {
        return Ok(Checkout::AdminAccess);
    }
    let price = stripe.cfg.price(mode).context("Choose an available billing plan")?;
    sqlx::query!("INSERT INTO billing_accounts (user_id) VALUES ($1) ON CONFLICT DO NOTHING", user.id)
        .execute(pool)
        .await?;
    let mut tx = pool.begin().await?;
    let account =
        sqlx::query!("SELECT customer_id, paid_at FROM billing_accounts WHERE user_id = $1 FOR UPDATE", user.id)
            .fetch_one(&mut *tx)
            .await?;
    let existing = sqlx::query!("SELECT id FROM billing_subscriptions WHERE user_id = $1 AND status NOT IN ('canceled', 'incomplete_expired') LIMIT 1", user.id).fetch_optional(&mut *tx).await?;
    if account.paid_at.is_some() || existing.is_some() {
        return Ok(Checkout::AlreadyPurchased);
    }
    // Commit the identity before contacting Stripe, so even a timeout can be retried safely.
    sqlx::query!("INSERT INTO billing_checkouts (id, user_id, mode, price_id, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_id) WHERE active DO NOTHING", Uuid::now_v7(), user.id, mode, price, user.email).execute(&mut *tx).await?;
    tx.commit().await?;
    let mut tx = pool.begin().await?;
    let account =
        sqlx::query!("SELECT customer_id, paid_at FROM billing_accounts WHERE user_id = $1 FOR UPDATE", user.id)
            .fetch_one(&mut *tx)
            .await?;
    let existing = sqlx::query!("SELECT id FROM billing_subscriptions WHERE user_id = $1 AND status NOT IN ('canceled', 'incomplete_expired') LIMIT 1", user.id).fetch_optional(&mut *tx).await?;
    if account.paid_at.is_some() || existing.is_some() {
        return Ok(Checkout::AlreadyPurchased);
    }
    let Some(row) = sqlx::query!(
        "SELECT id, mode, price_id, email, session_id, created_at FROM billing_checkouts WHERE user_id = $1 AND active",
        user.id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Checkout::Retry);
    };
    let attempt = Attempt { id: row.id, mode: row.mode, price_id: row.price_id, email: row.email };
    let session = if let Some(id) = row.session_id {
        stripe.session(&id).await?
    } else {
        ensure!(
            OffsetDateTime::now_utc() - row.created_at < time::Duration::hours(23),
            "Unresolved Checkout requires operator reconciliation"
        );
        stripe.create(user, &attempt, account.customer_id.as_deref(), site).await?
    };
    ensure!(
        session.client_reference_id == Some(user.id.to_string())
            && session.metadata.get(CHECKOUT_TAG) == Some(&attempt.id.to_string())
            && session.livemode == stripe.cfg.livemode
            && session.mode == attempt.mode,
        "Checkout identity mismatch"
    );
    sqlx::query!("UPDATE billing_checkouts SET session_id = $2 WHERE id = $1", attempt.id, session.id)
        .execute(&mut *tx)
        .await?;
    let result = match session.status.as_str() {
        "open" => {
            let url = session.url.context("Checkout URL missing")?;
            hosted_url(&url, "checkout.stripe.com")?;
            Checkout::Redirect(url)
        }
        "complete" => Checkout::Pending,
        "expired" => {
            sqlx::query!("UPDATE billing_checkouts SET active = false WHERE id = $1", attempt.id)
                .execute(&mut *tx)
                .await?;
            Checkout::Retry
        }
        _ => anyhow::bail!("Unexpected Checkout status"),
    };
    tx.commit().await?;
    Ok(result)
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    livemode: bool,
    account: Option<String>,
    data: EventData,
}
#[derive(Deserialize)]
struct EventData {
    object: serde_json::Value,
}
fn verify_signature(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let fields: Vec<_> = header.split(',').filter_map(|s| s.trim().split_once('=')).collect();
    let timestamps: Vec<_> = fields.iter().filter(|(k, _)| *k == "t").collect();
    if timestamps.len() != 1 {
        return false;
    }
    let timestamp = timestamps[0].1;
    let Ok(t) = timestamp.parse::<i64>() else { return false };
    if now.abs_diff(t) > 300 {
        return false;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC supports any key size");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    fields
        .iter()
        .filter(|(k, _)| *k == "v1")
        .any(|(_, signature)| hex::decode(signature).is_ok_and(|bytes| mac.clone().verify_slice(&bytes).is_ok()))
}

fn bad_webhook() -> AppError {
    AppError::Status(StatusCode::BAD_REQUEST, "Invalid Stripe webhook".into())
}

/// Fetch canonical objects while holding the account lock: duplicate and out-of-order
/// events converge on current Stripe state, including cancellations and failed renewals.
pub async fn webhook(pool: &PgPool, stripe: &StripeClient, signature: &str, body: &[u8]) -> Result<(), AppError> {
    if !verify_signature(&stripe.cfg.webhook_secret, signature, body, OffsetDateTime::now_utc().unix_timestamp()) {
        return Err(bad_webhook());
    }
    let event: Event = serde_json::from_slice(body).map_err(|_| bad_webhook())?;
    // This starter uses a direct Stripe account, not Connect.
    if event.livemode != stripe.cfg.livemode || event.account.is_some() {
        return Err(bad_webhook());
    }
    let object = &event.data.object;
    match event.kind.as_str() {
        "checkout.session.completed"
        | "checkout.session.async_payment_succeeded"
        | "checkout.session.async_payment_failed"
        | "checkout.session.expired" => {
            let Some(attempt_id) = object
                .pointer("/metadata/billing_checkout_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                return Ok(());
            };
            let Some(attempt) = sqlx::query!(
                "SELECT user_id, mode, price_id, session_id FROM billing_checkouts WHERE id = $1",
                attempt_id
            )
            .fetch_optional(pool)
            .await?
            else {
                return Ok(());
            };
            let id = object["id"].as_str().ok_or_else(bad_webhook)?;
            if attempt.session_id.as_deref().is_some_and(|saved| saved != id) {
                return Err(bad_webhook());
            }
            let mut tx = pool.begin().await?;
            let account =
                sqlx::query!("SELECT customer_id FROM billing_accounts WHERE user_id = $1 FOR UPDATE", attempt.user_id)
                    .fetch_one(&mut *tx)
                    .await?;
            let session = stripe.session(id).await?;
            if session.id != id
                || session.mode != attempt.mode
                || session.livemode != stripe.cfg.livemode
                || session.client_reference_id != Some(attempt.user_id.to_string())
                || session.metadata.get(CHECKOUT_TAG) != Some(&attempt_id.to_string())
            {
                return Err(bad_webhook());
            }
            if session.status == "expired"
                || (event.kind == "checkout.session.async_payment_failed" && session.payment_status == "unpaid")
            {
                sqlx::query!("UPDATE billing_checkouts SET active = false WHERE id = $1", attempt_id)
                    .execute(&mut *tx)
                    .await?;
            } else if session.status == "complete" {
                let items = session.line_items.as_ref().ok_or_else(bad_webhook)?;
                if !correct_items(items, &attempt.price_id, &attempt.mode, stripe.cfg.livemode) {
                    return Err(bad_webhook());
                }
                let customer = session.customer.as_deref().ok_or_else(bad_webhook)?;
                if account.customer_id.as_deref().is_some_and(|saved| saved != customer) {
                    return Err(bad_webhook());
                }
                sqlx::query!(
                    "UPDATE billing_accounts SET customer_id = $2 WHERE user_id = $1",
                    attempt.user_id,
                    customer
                )
                .execute(&mut *tx)
                .await?;
                if session.mode == "payment"
                    && matches!(session.payment_status.as_str(), "paid" | "no_payment_required")
                {
                    sqlx::query!(
                        "UPDATE billing_accounts SET paid_at = COALESCE(paid_at, now()) WHERE user_id = $1",
                        attempt.user_id
                    )
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query!("UPDATE billing_checkouts SET active = false WHERE id = $1", attempt_id)
                        .execute(&mut *tx)
                        .await?;
                }
                if let Some(id) = session.subscription.as_deref().filter(|_| session.mode == "subscription") {
                    let sub = stripe.subscription(id).await?;
                    save_subscription(&mut tx, stripe, &sub, attempt_id, attempt.user_id, &attempt.price_id, customer)
                        .await?;
                }
            }
            sqlx::query!("UPDATE billing_checkouts SET session_id = $2 WHERE id = $1", attempt_id, session.id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted"
        | "customer.subscription.paused"
        | "customer.subscription.resumed"
        | "invoice.paid"
        | "invoice.payment_failed"
        | "invoice.payment_action_required" => {
            let id = if event.kind.starts_with("invoice.") {
                // Dahlia invoice schema. One-off invoices have no subscription.
                object.pointer("/parent/subscription_details/subscription").and_then(|v| v.as_str())
            } else {
                object["id"].as_str()
            };
            let Some(id) = id else {
                return Ok(());
            };
            // First retrieval locates our durable attempt, even if the subscription event
            // arrives before checkout.session.completed. Never associate by email.
            let lookup = stripe.subscription(id).await?;
            let Some(attempt_id) = lookup.metadata.get(CHECKOUT_TAG).and_then(|s| Uuid::parse_str(s).ok()) else {
                return Ok(());
            };
            let Some(attempt) =
                sqlx::query!("SELECT user_id, mode, price_id FROM billing_checkouts WHERE id = $1", attempt_id)
                    .fetch_optional(pool)
                    .await?
            else {
                return Ok(());
            };
            if attempt.mode != "subscription" {
                return Err(bad_webhook());
            }
            let mut tx = pool.begin().await?;
            let account =
                sqlx::query!("SELECT customer_id FROM billing_accounts WHERE user_id = $1 FOR UPDATE", attempt.user_id)
                    .fetch_one(&mut *tx)
                    .await?;
            // Re-read AFTER locking; a concurrent newer event may already have committed.
            let sub = stripe.subscription(id).await?;
            if account.customer_id.as_ref().is_some_and(|saved| saved != &sub.customer) {
                return Err(bad_webhook());
            }
            save_subscription(&mut tx, stripe, &sub, attempt_id, attempt.user_id, &attempt.price_id, &sub.customer)
                .await?;
            sqlx::query!(
                "UPDATE billing_accounts SET customer_id = $2 WHERE user_id = $1",
                attempt.user_id,
                sub.customer
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        _ => {} // Acknowledge events outside this starter's billing lifecycle.
    }
    Ok(())
}
fn correct_items(items: &Items, price: &str, mode: &str, live: bool) -> bool {
    !items.has_more
        && items.data.len() == 1
        && items.data[0].quantity == Some(1)
        && items.data[0].price.id == price
        && items.data[0].price.livemode == live
        && items.data[0].price.kind == if mode == "subscription" { "recurring" } else { "one_time" }
}
async fn save_subscription(
    tx: &mut Transaction<'_, Postgres>,
    stripe: &StripeClient,
    sub: &Subscription,
    attempt: Uuid,
    user: Uuid,
    price: &str,
    customer: &str,
) -> Result<(), AppError> {
    if sub.livemode != stripe.cfg.livemode
        || sub.customer != customer
        || sub.metadata.get(CHECKOUT_TAG) != Some(&attempt.to_string())
    {
        return Err(bad_webhook());
    }
    // Stripe can mark asynchronous subscriptions active before payment settles.
    let access_active = correct_items(&sub.items, price, "subscription", stripe.cfg.livemode)
        && (sub.status == "trialing"
            || (sub.status == "active"
                && sub.latest_invoice.as_ref().is_some_and(|invoice| invoice.status.as_deref() == Some("paid"))));
    sqlx::query!("INSERT INTO billing_subscriptions (id, user_id, status, cancel_at_period_end, access_active) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, access_active = EXCLUDED.access_active, cancel_at_period_end = EXCLUDED.cancel_at_period_end, updated_at = now() WHERE billing_subscriptions.user_id = EXCLUDED.user_id", sub.id, user, sub.status, sub.cancel_at_period_end, access_active).execute(&mut **tx).await?;
    sqlx::query!("UPDATE billing_checkouts SET active = false WHERE id = $1", attempt).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
