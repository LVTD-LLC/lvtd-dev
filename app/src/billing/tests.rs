use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, header},
    response::{IntoResponse, Response},
    routing::any,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const SECRET: &str = "whsec_fixture";
fn config() -> Config {
    Config::parse("rk_test_fixture".into(), SECRET.into(), "price_recurring".into(), "price_once".into())
        .unwrap()
        .unwrap()
}
fn signature(body: &[u8], timestamp: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(format!("{timestamp}.").as_bytes());
    mac.update(body);
    format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()))
}
fn price(recurring: bool) -> Value {
    json!({"id": if recurring {"price_recurring"} else {"price_once"}, "active":true,"livemode":false,"type":if recurring {"recurring"} else {"one_time"}})
}
fn response(value: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], value.to_string()).into_response()
}
#[derive(Default)]
struct HeldResponse {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[derive(Default)]
struct Mock {
    hold_next: Option<Arc<HeldResponse>>,
    session: Value,
    subscription: Value,
    calls: Vec<(String, String, String)>,
    fail_create: bool,
    fail_get: bool,
    wrong_price: bool,
}
async fn mock(State(state): State<Arc<Mutex<Mock>>>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_owned();
    let method = req.method().to_string();
    let key = req.headers().get("idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    let body = String::from_utf8(req.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let (response, held) = {
        let mut mock = state.lock().unwrap();
        let held = mock.hold_next.take();
        (mock_response(&mut mock, &method, &path, key, body), held)
    };
    if let Some(held) = held {
        held.entered.notify_one();
        held.release.notified().await;
    }
    response
}
fn mock_response(mock: &mut Mock, method: &str, path: &str, key: String, body: String) -> Response {
    mock.calls.push((path.to_owned(), key, body.clone()));
    if method == "GET" && mock.fail_get {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    if path.starts_with("/v1/prices/") {
        let mut p = price(path.ends_with("price_recurring"));
        if mock.wrong_price {
            p["type"] = json!("wrong");
        }
        return response(p);
    }
    if path.starts_with("/v1/subscriptions/") {
        return response(mock.subscription.clone());
    }
    if path == "/v1/billing_portal/sessions" {
        return response(json!({"url":"https://billing.stripe.com/p/session/fixture"}));
    }
    if method == "POST" {
        let form: HashMap<_, _> =
            reqwest::Url::parse(&format!("http://local/?{body}")).unwrap().query_pairs().into_owned().collect();
        let recurring = form["mode"] == "subscription";
        let attempt = &form["metadata[billing_checkout_id]"];
        mock.session = json!({"id":format!("cs_test_{}",attempt.replace('-',"")),"url":"https://checkout.stripe.com/c/pay/fixture","status":"open","mode":form["mode"],"payment_status":"unpaid","livemode":false,"client_reference_id":form["client_reference_id"],"customer":"cus_fixture","subscription":if recurring {json!("sub_fixture")} else {Value::Null},"metadata":{"billing_checkout_id":attempt},"line_items":{"has_more":false,"data":[{"quantity":1,"price":price(recurring)}]}});
        mock.subscription = json!({"id":"sub_fixture","customer":"cus_fixture","status":"active","cancel_at_period_end":false,"latest_invoice":{"status":"paid"},"livemode":false,"metadata":{"billing_checkout_id":attempt},"items":{"has_more":false,"data":[{"quantity":1,"price":price(true)}]}});
        if mock.fail_create {
            mock.fail_create = false;
            return StatusCode::BAD_GATEWAY.into_response();
        }
    }
    response(mock.session.clone())
}
struct Fixture {
    pool: PgPool,
    client: StripeClient,
    app: Router,
    mock: Arc<Mutex<Mock>>,
    server: tokio::task::JoinHandle<()>,
    user: User,
    cookie: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let state = Arc::new(Mutex::new(Mock::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = StripeClient::new(config()).unwrap();
        client.base = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().fallback(any(mock)).with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let user = app_auth::users::sign_up(&pool, "buyer@example.com", "correct-horse".into()).await.unwrap();
        let token = app_auth::sessions::create(&pool, user.id, time::Duration::days(1)).await.unwrap();
        let cfg = app_core::Config {
            site_url: "http://test.local".into(),
            bind_addr: "127.0.0.1:0".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        };
        let (_, app) = crate::build_configured(
            cfg,
            pool.clone(),
            #[cfg(feature = "posthog")]
            None,
            #[cfg(feature = "sentry")]
            None,
            Some(client.clone()),
            #[cfg(feature = "storage")]
            crate::scratch_storage(),
            #[cfg(feature = "qdrant")]
            None,
        );
        Self { pool, client, app, mock: state, server, user, cookie: format!("session={token}") }
    }
    async fn send(&self, method: &str, path: &str, body: &str) -> (StatusCode, HeaderMap, String) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::COOKIE, &self.cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }
    async fn start(&self, mode: &str) {
        assert_eq!(self.send("POST", "/billing", &format!("mode={mode}")).await.0, StatusCode::SEE_OTHER);
    }
    fn complete(&self, paid: bool) {
        let mut mock = self.mock.lock().unwrap();
        mock.session["status"] = json!("complete");
        mock.session["payment_status"] = json!(if paid { "paid" } else { "unpaid" });
    }
    async fn event(&self, kind: &str) -> StatusCode {
        let object = {
            let m = self.mock.lock().unwrap();
            if kind.starts_with("invoice.") {
                json!({"parent":{"subscription_details":{"subscription":"sub_fixture"}}})
            } else if kind.starts_with("customer.") {
                m.subscription.clone()
            } else {
                m.session.clone()
            }
        };
        self.deliver(kind, object).await
    }
    async fn deliver(&self, kind: &str, object: Value) -> StatusCode {
        let body = serde_json::to_vec(&json!({"type":kind,"livemode":false,"data":{"object":object}})).unwrap();
        let sig = signature(&body, OffsetDateTime::now_utc().unix_timestamp());
        self.app
            .clone()
            .oneshot(
                Request::post("/stripe/webhook")
                    .header("stripe-signature", sig)
                    .header(header::ORIGIN, "https://stripe.com")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
    async fn access(&self) -> bool {
        status(&self.pool, self.user.id).await.unwrap().has_access()
    }
}

#[test]
fn signatures_require_unchanged_body_recent_timestamp_and_correct_secret() {
    let body = b"original";
    let sig = signature(body, 1000);
    assert!(verify_signature(SECRET, &format!("{sig},v1=abcd"), body, 1000));
    for (data, now) in [(b"changed".as_slice(), 1000), (body.as_slice(), 1301), (body.as_slice(), 699)] {
        assert!(!verify_signature(SECRET, &sig, data, now));
    }
    assert!(!verify_signature(SECRET, &format!("{sig},t=1000"), body, 1000));
    assert!(!verify_signature("wrong", &sig, body, 1000));
}
#[test]
fn configuration_and_access_policy_are_explicit() {
    assert!(Config::parse("".into(), "".into(), "".into(), "".into()).unwrap().is_none());
    assert!(Config::parse("sk_test_x".into(), "".into(), "price_x".into(), "".into()).is_err());
    assert!(Config::parse("sk_test_x".into(), SECRET.into(), "".into(), "".into()).is_err());
    assert!(Config::parse("sk_test_x".into(), SECRET.into(), "price_x/evil".into(), "".into()).is_err());
    for state in ["active", "trialing", "past_due", "unpaid", "canceled", "incomplete", "incomplete_expired", "paused"]
    {
        let mut s = BillingStatus {
            is_admin: false,
            paid: false,
            subscription_access: matches!(state, "active" | "trialing"),
            subscription_status: Some(state.into()),
            cancel_at_period_end: true,
            has_customer: true,
        };
        assert_eq!(s.has_access(), matches!(state, "active" | "trialing"));
        s.is_admin = true;
        assert!(s.has_access());
        s.is_admin = false;
        s.paid = true;
        assert!(s.has_access());
    }
}
#[sqlx::test(migrations = "../migrations")]
async fn checkout_modes_are_server_selected_and_retries_reuse_the_same_attempt(pool: PgPool) {
    let f = Fixture::new(pool).await;
    assert_eq!(
        f.send("POST", "/billing", "mode=invalid&price_id=price_free").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    f.mock.lock().unwrap().fail_create = true;
    assert_eq!(f.send("POST", "/billing", "mode=subscription").await.0, StatusCode::BAD_GATEWAY);
    let (a, b) =
        tokio::join!(f.send("POST", "/billing", "mode=subscription"), f.send("POST", "/billing", "mode=payment"));
    assert_eq!(a.0, StatusCode::SEE_OTHER);
    assert_eq!(a.1[header::LOCATION], b.1[header::LOCATION]);
    let m = f.mock.lock().unwrap();
    let creates: Vec<_> = m.calls.iter().filter(|c| !c.1.is_empty()).collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0].1, creates[1].1);
    let form: HashMap<_, _> =
        reqwest::Url::parse(&format!("http://local/?{}", creates[0].2)).unwrap().query_pairs().into_owned().collect();
    assert_eq!(form["mode"], "subscription");
    assert_eq!(form["line_items[0][price]"], "price_recurring");
    assert_eq!(form["subscription_data[metadata][billing_checkout_id]"], form["metadata[billing_checkout_id]"]);
    assert!(!form.contains_key("payment_method_types"));
}
#[sqlx::test(migrations = "../migrations")]
async fn delayed_one_time_payments_only_grant_access_when_paid(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("payment").await;
    f.complete(false);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(!f.access().await);
    assert_eq!(f.send("GET", "/billing/success?paid=true", "").await.0, StatusCode::OK);
    assert!(!f.access().await);
    assert_eq!(f.event("checkout.session.async_payment_failed").await, StatusCode::OK);
    assert!(!f.access().await);
    f.complete(true);
    let (a, b) =
        tokio::join!(f.event("checkout.session.async_payment_succeeded"), f.event("checkout.session.completed"));
    assert_eq!((a, b), (StatusCode::OK, StatusCode::OK));
    assert!(f.access().await);
    let calls = f.mock.lock().unwrap().calls.len();
    assert_eq!(f.send("POST", "/billing", "mode=payment").await.0, StatusCode::OK);
    assert_eq!(calls, f.mock.lock().unwrap().calls.len());
    assert_eq!(
        f.send("POST", "/billing/portal", "").await.1[header::LOCATION],
        "https://billing.stripe.com/p/session/fixture"
    );
    assert!(f.mock.lock().unwrap().calls.last().unwrap().2.contains("customer=cus_fixture"));
}
#[sqlx::test(migrations = "../migrations")]
async fn subscriptions_track_renewals_failure_recovery_and_cancellation(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    // Subscription arrives before the Checkout event.
    assert_eq!(f.event("customer.subscription.created").await, StatusCode::OK);
    assert!(f.access().await);
    let old = f.mock.lock().unwrap().subscription.clone();
    f.mock.lock().unwrap().subscription["status"] = json!("past_due");
    assert_eq!(f.event("invoice.payment_failed").await, StatusCode::OK);
    assert!(!f.access().await);
    // A late active event must not restore access to a now past-due subscription.
    assert_eq!(f.deliver("customer.subscription.updated", old.clone()).await, StatusCode::OK);
    assert!(!f.access().await);
    assert_eq!(f.send("POST", "/billing", "mode=subscription").await.0, StatusCode::OK);
    f.mock.lock().unwrap().subscription["status"] = json!("active");
    assert_eq!(f.event("invoice.paid").await, StatusCode::OK);
    assert!(f.access().await);
    f.mock.lock().unwrap().subscription["cancel_at_period_end"] = json!(true);
    assert_eq!(f.event("customer.subscription.updated").await, StatusCode::OK);
    assert!(f.access().await);
    assert!(f.send("GET", "/billing", "").await.2.contains("end of the current billing period"));
    f.mock.lock().unwrap().subscription["status"] = json!("canceled");
    assert_eq!(f.event("customer.subscription.deleted").await, StatusCode::OK);
    assert!(!f.access().await);
    assert_eq!(f.deliver("customer.subscription.updated", old).await, StatusCode::OK);
    assert!(!f.access().await);
    f.start("subscription").await;
}
#[sqlx::test(migrations = "../migrations")]
async fn mismatched_sessions_do_not_fulfill_and_failures_are_retryable(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("payment").await;
    f.complete(true);
    let original = f.mock.lock().unwrap().session.clone();
    for (path, value) in [
        ("/client_reference_id", json!(Uuid::now_v7())),
        ("/livemode", json!(true)),
        ("/mode", json!("subscription")),
        ("/line_items/data/0/price/id", json!("price_wrong")),
        ("/line_items/data/0/quantity", json!(2)),
    ] {
        {
            let mut m = f.mock.lock().unwrap();
            m.session = original.clone();
            *m.session.pointer_mut(path).unwrap() = value;
        }
        assert_eq!(f.event("checkout.session.completed").await, StatusCode::BAD_REQUEST, "{path}");
        assert!(!f.access().await);
    }
    {
        let mut m = f.mock.lock().unwrap();
        m.session = original;
        m.fail_get = true;
    }
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!f.access().await);
    f.mock.lock().unwrap().fail_get = false;
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(f.access().await);
}
#[sqlx::test(migrations = "../migrations")]
async fn expired_and_lost_sessions_are_handled_without_duplicate_charges(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("payment").await;
    f.mock.lock().unwrap().session["status"] = json!("expired");
    assert_eq!(f.event("checkout.session.expired").await, StatusCode::OK);
    f.start("payment").await;
    sqlx::query!("UPDATE billing_checkouts SET session_id = NULL, created_at = now() - interval '24 hours' WHERE user_id = $1 AND active",f.user.id).execute(&f.pool).await.unwrap();
    let calls = f.mock.lock().unwrap().calls.len();
    assert_eq!(f.send("POST", "/billing", "mode=payment").await.0, StatusCode::BAD_GATEWAY);
    assert_eq!(calls, f.mock.lock().unwrap().calls.len());
    f.complete(true);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(f.access().await);
}
#[sqlx::test(migrations = "../migrations")]
async fn billing_is_authenticated_origin_protected_and_validates_configuration(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for (method, path) in
        [("GET", "/billing"), ("POST", "/billing"), ("GET", "/billing/success"), ("POST", "/billing/portal")]
    {
        let res = f
            .app
            .clone()
            .oneshot(Request::builder().method(method).uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(res.headers()[header::LOCATION], "/sign-in");
    }
    let res = f
        .app
        .clone()
        .oneshot(
            Request::post("/billing")
                .header(header::COOKIE, &f.cookie)
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = f.app.clone().oneshot(Request::post("/stripe/webhook").body(Body::from("{}")).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    f.client.validate().await.unwrap();
    f.mock.lock().unwrap().wrong_price = true;
    assert!(f.client.validate().await.is_err());
    assert_eq!(f.deliver("unrelated.event", json!({})).await, StatusCode::OK);
    assert!(!f.access().await);
}

#[sqlx::test(migrations = "../migrations")]
async fn active_subscription_waits_for_async_invoice_settlement(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    f.mock.lock().unwrap().subscription["latest_invoice"]["status"] = json!("open");
    assert_eq!(f.event("customer.subscription.created").await, StatusCode::OK);
    assert!(!f.access().await);
    assert_eq!(f.event("invoice.payment_failed").await, StatusCode::OK);
    assert!(!f.access().await);
    f.mock.lock().unwrap().subscription["latest_invoice"]["status"] = json!("paid");
    assert_eq!(f.event("invoice.paid").await, StatusCode::OK);
    assert!(f.access().await);
}

#[sqlx::test(migrations = "../migrations")]
async fn subscription_pause_and_unsupported_price_remove_access(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    assert_eq!(f.event("customer.subscription.created").await, StatusCode::OK);
    assert!(f.access().await);
    f.mock.lock().unwrap().subscription["status"] = json!("paused");
    assert_eq!(f.event("customer.subscription.paused").await, StatusCode::OK);
    assert!(!f.access().await);
    f.mock.lock().unwrap().subscription["status"] = json!("active");
    assert_eq!(f.event("customer.subscription.resumed").await, StatusCode::OK);
    assert!(f.access().await);
    f.mock.lock().unwrap().subscription["items"]["data"][0]["price"]["id"] = json!("price_unrecognized");
    assert_eq!(f.event("customer.subscription.updated").await, StatusCode::OK);
    assert!(!f.access().await);
    f.mock.lock().unwrap().subscription["status"] = json!("canceled");
    assert_eq!(f.event("customer.subscription.deleted").await, StatusCode::OK);
    assert_eq!(status(&f.pool, f.user.id).await.unwrap().subscription_status.as_deref(), Some("canceled"));
}

#[sqlx::test(migrations = "../migrations")]
async fn checkout_retry_after_email_change_keeps_original_request(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.mock.lock().unwrap().fail_create = true;
    assert_eq!(f.send("POST", "/billing", "mode=payment").await.0, StatusCode::BAD_GATEWAY);
    assert_eq!(
        f.send("POST", "/settings", "action=profile&name=Buyer&email=new@example.com&password=correct-horse").await.0,
        StatusCode::SEE_OTHER
    );
    f.start("payment").await;
    let m = f.mock.lock().unwrap();
    let creates: Vec<_> = m.calls.iter().filter(|c| !c.1.is_empty()).collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0].1, creates[1].1);
    assert_eq!(creates[0].2, creates[1].2);
    assert!(creates[1].2.contains("customer_email=buyer%40example.com"));
}

#[sqlx::test(migrations = "../migrations")]
async fn admin_access_tracks_live_role_without_creating_billing_records(pool: PgPool) {
    let f = Fixture::new(pool).await;
    assert!(require_access(&f.pool, f.user.id).await.is_err());
    #[cfg(feature = "api")]
    let (_, key) = crate::api_keys::create(&f.pool, f.user.id, "before-promotion").await.unwrap();
    crate::admin::set_admin(&f.pool, &f.user.email, true).await.unwrap();
    assert!(status(&f.pool, f.user.id).await.unwrap().has_access());
    assert!(!status(&f.pool, f.user.id).await.unwrap().paid);
    assert!(require_access(&f.pool, f.user.id).await.is_ok());
    #[cfg(feature = "api")]
    {
        let user = crate::api_keys::authenticate(&f.pool, &key).await.unwrap().unwrap();
        assert!(user.is_admin);
        assert!(require_access(&f.pool, user.id).await.is_ok());
    }
    let (_, _, page) = f.send("GET", "/billing", "").await;
    assert!(page.contains("Admin access"));
    assert!(!page.contains("Choose a plan"));
    assert!(!page.contains("name=\"mode\""));
    assert!(!page.contains("Your one-time purchase is confirmed"));
    for mode in ["payment", "subscription", "invalid"] {
        assert_eq!(f.send("POST", "/billing", &format!("mode={mode}")).await.0, StatusCode::OK);
    }
    // The helper must re-read the role, not trust this pre-promotion User snapshot.
    assert!(matches!(
        checkout(&f.pool, &f.client, &f.user, "payment", "http://test.local").await.unwrap(),
        Checkout::AdminAccess
    ));
    assert!(f.mock.lock().unwrap().calls.is_empty());
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"count!\" FROM billing_accounts WHERE user_id = $1", f.user.id)
            .fetch_one(&f.pool)
            .await
            .unwrap()
            .count,
        0
    );
    assert_eq!(
        sqlx::query!("SELECT count(*) AS \"count!\" FROM billing_checkouts WHERE user_id = $1", f.user.id)
            .fetch_one(&f.pool)
            .await
            .unwrap()
            .count,
        0
    );
    crate::admin::set_admin(&f.pool, &f.user.email, false).await.unwrap();
    assert!(!f.access().await);
    assert!(require_access(&f.pool, f.user.id).await.is_err());
    #[cfg(feature = "api")]
    {
        let user = crate::api_keys::authenticate(&f.pool, &key).await.unwrap().unwrap();
        assert!(!user.is_admin);
        assert!(require_access(&f.pool, user.id).await.is_err());
    }
    assert!(f.send("GET", "/billing", "").await.2.contains("name=\"mode\""));
}

#[sqlx::test(migrations = "../migrations")]
async fn admin_role_preserves_existing_subscription_and_portal(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    f.complete(true);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    crate::admin::set_admin(&f.pool, &f.user.email, true).await.unwrap();
    let (_, _, page) = f.send("GET", "/billing", "").await;
    assert!(page.contains("Admin access") && page.contains("Manage billing"));
    assert_eq!(f.send("POST", "/billing/portal", "").await.0, StatusCode::SEE_OTHER);
    crate::admin::set_admin(&f.pool, &f.user.email, false).await.unwrap();
    assert!(f.access().await);
    f.mock.lock().unwrap().subscription["status"] = json!("canceled");
    assert_eq!(f.event("customer.subscription.deleted").await, StatusCode::OK);
    assert!(!f.access().await);
    crate::admin::set_admin(&f.pool, &f.user.email, true).await.unwrap();
    assert!(f.access().await);
}

#[sqlx::test(migrations = "../migrations")]
async fn unrelated_events_are_ignored_without_provider_calls_or_billing_changes(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    let original = f.mock.lock().unwrap().session.clone();
    for (kind, object, expected) in [
        ("unrelated.event", original.clone(), StatusCode::OK),
        ("invoice.paid", json!({}), StatusCode::OK),
        ("customer.subscription.updated", json!({}), StatusCode::OK),
        ("checkout.session.completed", json!({}), StatusCode::OK),
        ("checkout.session.completed", json!({"metadata":{"billing_checkout_id":"invalid"}}), StatusCode::OK),
        ("checkout.session.completed", json!({"metadata":{"billing_checkout_id":Uuid::now_v7()}}), StatusCode::OK),
        (
            "checkout.session.completed",
            json!({"id":"cs_other", "metadata":original["metadata"]}),
            StatusCode::BAD_REQUEST,
        ),
        ("checkout.session.completed", json!({"metadata":original["metadata"]}), StatusCode::BAD_REQUEST),
    ] {
        let calls = f.mock.lock().unwrap().calls.len();
        assert_eq!(f.deliver(kind, object).await, expected, "{kind}");
        assert_eq!(f.mock.lock().unwrap().calls.len(), calls);
        let billing = status(&f.pool, f.user.id).await.unwrap();
        assert!(!billing.has_access());
        assert!(!billing.has_customer);
        assert!(billing.subscription_status.is_none());
    }
    f.complete(true);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(f.access().await);
}

#[sqlx::test(migrations = "../migrations")]
async fn canonical_checkout_rejections_roll_back_customer_and_purchase(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("payment").await;
    f.complete(true);
    let original = f.mock.lock().unwrap().session.clone();
    for (pointer, value, expected) in [
        ("/id", json!("cs_other"), StatusCode::BAD_REQUEST),
        ("/metadata/billing_checkout_id", json!(Uuid::now_v7()), StatusCode::BAD_REQUEST),
        ("/status", json!("open"), StatusCode::OK),
        ("/customer", Value::Null, StatusCode::BAD_REQUEST),
        ("/line_items", Value::Null, StatusCode::BAD_REQUEST),
        ("/line_items/has_more", json!(true), StatusCode::BAD_REQUEST),
        ("/line_items/data", json!([]), StatusCode::BAD_REQUEST),
        (
            "/line_items/data",
            json!([{"quantity":1,"price":price(false)},{"quantity":1,"price":price(false)}]),
            StatusCode::BAD_REQUEST,
        ),
        ("/line_items/data/0/price/livemode", json!(true), StatusCode::BAD_REQUEST),
        ("/line_items/data/0/price/type", json!("recurring"), StatusCode::BAD_REQUEST),
    ] {
        {
            let mut mock = f.mock.lock().unwrap();
            mock.session = original.clone();
            *mock.session.pointer_mut(pointer).unwrap() = value;
        }
        assert_eq!(f.deliver("checkout.session.completed", original.clone()).await, expected, "{pointer}");
        let billing = status(&f.pool, f.user.id).await.unwrap();
        assert!(!billing.has_access(), "{pointer}");
        assert!(!billing.has_customer, "{pointer}: failed validation must not associate a customer");
        assert!(billing.subscription_status.is_none());
    }
    f.mock.lock().unwrap().session = original;
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(f.access().await);
}

#[sqlx::test(migrations = "../migrations")]
async fn subscription_events_validate_ownership_and_preserve_access_on_rejection(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    f.complete(true);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    let original = f.mock.lock().unwrap().subscription.clone();
    for (pointer, value, expected) in [
        ("/metadata", json!({}), StatusCode::OK),
        ("/metadata/billing_checkout_id", json!(Uuid::now_v7()), StatusCode::OK),
        ("/customer", json!("cus_other"), StatusCode::BAD_REQUEST),
        ("/livemode", json!(true), StatusCode::BAD_REQUEST),
    ] {
        {
            let mut mock = f.mock.lock().unwrap();
            mock.subscription = original.clone();
            mock.subscription["status"] = json!("canceled");
            *mock.subscription.pointer_mut(pointer).unwrap() = value;
        }
        assert_eq!(f.deliver("customer.subscription.deleted", original.clone()).await, expected, "{pointer}");
        let billing = status(&f.pool, f.user.id).await.unwrap();
        assert!(billing.has_access(), "{pointer}: invalid cancellation cannot revoke access");
        assert_eq!(billing.subscription_status.as_deref(), Some("active"));
        assert!(!billing.paid, "subscription is not a permanent purchase");
    }
    {
        let mut mock = f.mock.lock().unwrap();
        mock.subscription = original;
        mock.subscription["status"] = json!("trialing");
        mock.subscription["latest_invoice"] = Value::Null;
    }
    assert_eq!(f.event("customer.subscription.updated").await, StatusCode::OK);
    assert!(f.access().await, "a trial does not need a paid invoice");
    f.mock.lock().unwrap().subscription["status"] = json!("active");
    assert_eq!(f.event("customer.subscription.updated").await, StatusCode::OK);
    assert!(!f.access().await, "active without an invoice is not proof of payment");
    f.mock.lock().unwrap().subscription["latest_invoice"] = json!({"status":"paid"});
    assert_eq!(f.event("invoice.paid").await, StatusCode::OK);
    assert!(f.access().await, "canonical settlement restores access");
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_checkout_modes_and_retries_share_one_durable_attempt(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.mock.lock().unwrap().fail_create = true;
    let (a, b, c, d) = tokio::join!(
        f.send("POST", "/billing", "mode=payment"),
        f.send("POST", "/billing", "mode=subscription"),
        f.send("POST", "/billing", "mode=payment"),
        f.send("POST", "/billing", "mode=subscription"),
    );
    let results = [a, b, c, d];
    assert_eq!(results.iter().filter(|r| r.0 == StatusCode::BAD_GATEWAY).count(), 1);
    for result in results.iter().filter(|r| r.0 != StatusCode::BAD_GATEWAY) {
        assert_eq!(result.0, StatusCode::SEE_OTHER);
        assert_eq!(result.1[header::LOCATION], "https://checkout.stripe.com/c/pay/fixture");
    }
    let attempts = sqlx::query!(
        "SELECT id, session_id, active, mode, price_id FROM billing_checkouts WHERE user_id = $1",
        f.user.id
    )
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(attempts[0].active);
    {
        let mock = f.mock.lock().unwrap();
        let creates: Vec<_> = mock.calls.iter().filter(|c| c.0 == "/v1/checkout/sessions").collect();
        assert_eq!(creates.len(), 2);
        assert_eq!(creates[0], creates[1], "retry key, mode, price and email must remain identical");
        assert!(!creates[0].1.is_empty());
        assert_eq!(attempts[0].session_id.as_deref(), mock.session["id"].as_str());
        assert_eq!(attempts[0].mode, mock.session["mode"]);
        assert_eq!(attempts[0].price_id, mock.session["line_items"]["data"][0]["price"]["id"]);
    }
    assert!(!f.access().await);
    let billing = status(&f.pool, f.user.id).await.unwrap();
    assert!(!billing.paid && !billing.has_customer && billing.subscription_status.is_none());
}

#[sqlx::test(migrations = "../migrations")]
async fn stale_subscription_lookup_cannot_restore_access_after_concurrent_cancellation(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("subscription").await;
    f.complete(true);
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    assert!(f.access().await);
    let held = Arc::new(HeldResponse::default());
    f.mock.lock().unwrap().hold_next = Some(held.clone());
    // Capture an active subscription in an older event's identity lookup, then hold its response.
    let stale = f.event("invoice.paid");
    tokio::pin!(stale);
    tokio::select! {
        _ = held.entered.notified() => {},
        result = &mut stale => panic!("event escaped held lookup: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("provider was never reached"),
    }
    f.mock.lock().unwrap().subscription["status"] = json!("canceled");
    assert_eq!(f.event("customer.subscription.deleted").await, StatusCode::OK);
    assert!(!f.access().await);
    let calls = f.mock.lock().unwrap().calls.len();
    held.release.notify_one();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), stale).await.unwrap(), StatusCode::OK);
    assert_eq!(f.mock.lock().unwrap().calls.len(), calls + 1, "re-read canonical state after acquiring the lock");
    let billing = status(&f.pool, f.user.id).await.unwrap();
    assert_eq!(billing.subscription_status.as_deref(), Some("canceled"));
    assert!(!billing.has_access() && !billing.paid);
    let subscriptions = sqlx::query!("SELECT id, status FROM billing_subscriptions WHERE user_id = $1", f.user.id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].id, "sub_fixture");
    assert_eq!(subscriptions[0].status, "canceled");
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_one_time_webhooks_preserve_the_first_purchase(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.start("payment").await;
    f.complete(true);
    let (a, b, c) = tokio::join!(
        f.event("checkout.session.completed"),
        f.event("checkout.session.async_payment_succeeded"),
        f.event("checkout.session.completed")
    );
    assert_eq!([a, b, c], [StatusCode::OK; 3]);
    let first = sqlx::query!("SELECT customer_id, paid_at FROM billing_accounts WHERE user_id = $1", f.user.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(first.customer_id.as_deref(), Some("cus_fixture"));
    assert!(first.paid_at.is_some());
    assert_eq!(f.event("checkout.session.completed").await, StatusCode::OK);
    let replayed = sqlx::query!("SELECT customer_id, paid_at FROM billing_accounts WHERE user_id = $1", f.user.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!((replayed.customer_id, replayed.paid_at), (first.customer_id, first.paid_at));
    let attempts = sqlx::query!("SELECT active FROM billing_checkouts WHERE user_id = $1", f.user.id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(!attempts[0].active);
    assert!(f.access().await);
    let calls = f.mock.lock().unwrap().calls.len();
    assert!(matches!(
        checkout(&f.pool, &f.client, &f.user, "subscription", "http://test.local").await.unwrap(),
        Checkout::AlreadyPurchased
    ));
    assert_eq!(f.mock.lock().unwrap().calls.len(), calls);
}
