#![cfg(feature = "auth")]
use lvtd_dev as app;

use app_core::Config;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

#[sqlx::test(migrations = "../migrations")]
async fn signup_never_authenticates_without_email_delivery(pool: sqlx::PgPool) {
    let cfg = Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let app = app::build(cfg, pool.clone()).1;
    let response = app
        .clone()
        .oneshot(
            Request::post("/sign-up")
                .header(header::ORIGIN, "http://test.local")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("email=new%40example.com&password=correct-horse"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
}

mod common;
use http_body_util::BodyExt;
fn app(pool: sqlx::PgPool) -> (app::SharedState, axum::Router) {
    common::mail_app(app::build(
        Config {
            bind_addr: "127.0.0.1:0".into(),
            site_url: "http://test.local".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        },
        pool,
    ))
}
async fn post(app: &axum::Router, path: &str, body: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(path)
                .header(header::ORIGIN, "http://test.local")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, cookie)
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
fn cookie(response: &axum::response::Response) -> String {
    response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().into()
}
async fn signup(app: &axum::Router, email: &str) -> String {
    let r = post(app, "/sign-up", &format!("email={email}&password=correct-horse"), "").await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[header::LOCATION], "/verify-email");
    let c = cookie(&r);
    assert!(c.starts_with("signup="));
    c
}
#[sqlx::test(migrations = "../migrations")]
async fn pending_cannot_sign_in_then_code_is_single_use_and_browser_bound(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let c = signup(&app, "flow@example.com").await;
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
    assert_eq!(
        post(&app, "/sign-in", "email=flow@example.com&password=correct-horse", "").await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let dashboard = app
        .clone()
        .oneshot(Request::get("/dashboard").header(header::COOKIE, &c).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(dashboard.headers()[header::LOCATION], "/sign-in");
    let code = common::code(&app, "flow@example.com").await;
    assert_eq!(code.len(), 6);
    assert_eq!(
        post(&app, "/verify-email", &format!("code={code}"), &format!("signup={}", "a".repeat(64))).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let verified = common::complete(&app, &c, "flow@example.com").await;
    assert_eq!(verified.status(), StatusCode::SEE_OTHER);
    assert!(cookie(&verified).starts_with("session="));
    assert_eq!(common::complete(&app, &c, "flow@example.com").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
        Some(0)
    );
}
#[sqlx::test(migrations = "../migrations")]
async fn cooldown_rotation_expiry_and_attempt_limit(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let c = signup(&app, "rotation@example.com").await;
    let old = common::code(&app, "rotation@example.com").await;
    assert_eq!(post(&app, "/verify-email/resend", "", &c).await.status(), StatusCode::TOO_MANY_REQUESTS);
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&app, "/verify-email/resend", "", &c).await.status(), StatusCode::OK);
    let new = common::code(&app, "rotation@example.com").await;
    assert_ne!(old, new);
    assert_eq!(
        post(&app, "/verify-email", &format!("code={old}"), &c).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for _ in 0..4 {
        assert_eq!(post(&app, "/verify-email", "code=!!!!!!", &c).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(common::complete(&app, &c, "rotation@example.com").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    sqlx::query!("UPDATE pending_signups SET attempts = 0, expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(common::complete(&app, &c, "rotation@example.com").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
#[sqlx::test(migrations = "../migrations")]
async fn concurrent_consumption_has_one_winner_and_csrf_is_required(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let c = signup(&app, "race@example.com").await;
    let code = common::code(&app, "race@example.com").await;
    for origin in [None, Some("https://evil.example")] {
        let mut request = Request::post("/verify-email")
            .header(header::COOKIE, &c)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let r = app.clone().oneshot(request.body(Body::from(format!("code={code}"))).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }
    let (a, b) =
        tokio::join!(common::complete(&app, &c, "race@example.com"), common::complete(&app, &c, "race@example.com"));
    assert_eq!([a.status(), b.status()].iter().filter(|&&s| s == StatusCode::SEE_OTHER).count(), 1);
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(1));
}
#[sqlx::test(migrations = "../migrations")]
async fn replacement_signup_cannot_use_an_older_browser_or_password(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let old_cookie = signup(&app, "replacement@example.com").await;
    let old_code = common::code(&app, "replacement@example.com").await;
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let new_cookie =
        cookie(&post(&app, "/sign-up", "email=replacement@example.com&password=new-correct-horse", "").await);
    assert_ne!(old_cookie, new_cookie);
    assert_eq!(
        post(&app, "/verify-email", &format!("code={old_code}"), &old_cookie).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(common::complete(&app, &new_cookie, "replacement@example.com").await.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        post(&app, "/sign-in", "email=replacement@example.com&password=correct-horse", "").await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        post(&app, "/sign-in", "email=replacement@example.com&password=new-correct-horse", "").await.status(),
        StatusCode::SEE_OTHER
    );
}
#[sqlx::test(migrations = "../migrations")]
async fn existing_account_is_not_modified_and_email_budget_persists(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    app_auth::users::sign_up(&pool, "existing@example.com", "original-password".into()).await.unwrap();
    let c = signup(&app, "existing@example.com").await;
    assert_eq!(post(&app, "/verify-email", "code=123456", &c).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        app_auth::users::verify_credentials(&pool, "existing@example.com", "original-password".into())
            .await
            .unwrap()
            .is_some()
    );
    for _ in 0..4 {
        signup(&app, "existing@example.com").await;
    }
    let (state2, app2) = self::app(pool);
    assert_eq!(
        post(&app2, "/sign-up", "email=existing@example.com&password=correct-horse", "").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(state2);
}
#[sqlx::test(migrations = "../migrations")]
async fn provider_failure_does_not_create_account_and_allows_later_retry(pool: sqlx::PgPool) {
    let (state, router) = app(pool.clone());
    drop(router);
    let mut state = state;
    std::sync::Arc::get_mut(&mut state).unwrap().email = Some(
        app::verification::Client::with_endpoint(
            app::verification::Config {
                api_key: "test".into(),
                domain: "mg.example.com".into(),
                from: "noreply@example.com".into(),
                hash_key: "test-only-long-key-not-production-12345".into(),
            },
            "http://127.0.0.1:1/messages".into(),
        )
        .unwrap(),
    );
    let app = app::routes::router(state);
    let r = post(&app, "/sign-up", "email=failure@example.com&password=correct-horse", "").await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let c = cookie(&r);
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("couldn’t send"));
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
    assert!(!sqlx::query!("SELECT delivered FROM pending_signups").fetch_one(&pool).await.unwrap().delivered);
    let (_, working) = self::app(pool.clone());
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&working, "/verify-email/resend", "", &c).await.status(), StatusCode::OK);
    assert_eq!(common::complete(&working, &c, "failure@example.com").await.status(), StatusCode::SEE_OTHER);
}

#[sqlx::test(migrations = "../migrations")]
async fn recipient_injection_is_rejected_before_delivery(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    for email in [
        "victim%40example.com%0D%0ABcc%3Ax",
        "Name%3Cvictim%40example.com%3E",
        "one%40example.com,two%40example.com",
        "a%40example..com",
    ] {
        assert_eq!(
            post(&app, "/sign-up", &format!("email={email}&password=correct-horse"), "").await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
        Some(0)
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn all_signup_posts_require_origin_and_cookies_are_secure_on_https(pool: sqlx::PgPool) {
    let (_, app) = common::mail_app(app::build(
        Config {
            bind_addr: "127.0.0.1:0".into(),
            site_url: "https://test.local".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        },
        pool.clone(),
    ));
    for path in ["/sign-up", "/verify-email", "/verify-email/resend"] {
        for origin in [None, Some("https://evil.example")] {
            let mut request = Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            let response = app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }
    let response = app
        .clone()
        .oneshot(
            Request::post("/sign-up")
                .header(header::ORIGIN, "https://test.local")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("email=secure%40example.com&password=correct-horse"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let pending_cookie = cookie(&response);
    let header = response.headers()[header::SET_COOKIE].to_str().unwrap();
    for attribute in ["HttpOnly", "SameSite=Strict", "Secure"] {
        assert!(header.contains(attribute));
    }
    let response = app
        .clone()
        .oneshot(
            Request::post("/verify-email")
                .header(header::ORIGIN, "https://test.local")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, pending_cookie)
                .body(Body::from(format!(
                    "code={}",
                    common::code(&app, "secure@example.com").await.to_ascii_lowercase()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookies: Vec<_> = response.headers().get_all(header::SET_COOKIE).iter().map(|c| c.to_str().unwrap()).collect();
    assert!(cookies.iter().any(|c| c.starts_with("session=") && c.contains("Secure") && c.contains("HttpOnly")));
    assert!(cookies.iter().any(|c| c.starts_with("signup=;") && c.contains("Max-Age=0")));
}

#[sqlx::test(migrations = "../migrations")]
async fn console_transport_logs_the_code_and_completes_signup(pool: sqlx::PgPool) {
    use std::sync::{Arc, Mutex};
    use tracing::instrument::WithSubscriber;
    let (mut state, router) = app(pool.clone());
    drop(router);
    Arc::get_mut(&mut state).unwrap().email =
        Some(app::verification::Client::console("test-only-long-key-not-production-12345".into()));
    let app = app::routes::router(state);
    #[derive(Clone)]
    struct Log(Arc<Mutex<Vec<u8>>>);
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
        type Writer = Log;
        fn make_writer(&'a self) -> Log {
            self.clone()
        }
    }
    impl std::io::Write for Log {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Log(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt().with_writer(log.clone()).with_ansi(false).finish();
    let r = post(&app, "/sign-up", "email=console%40example.com&password=correct-horse", "")
        .with_subscriber(subscriber)
        .await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let c = cookie(&r);
    assert!(sqlx::query!("SELECT delivered FROM pending_signups").fetch_one(&pool).await.unwrap().delivered);
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("To: console@example.com"), "{text}");
    let code = text.split("code is: ").nth(1).and_then(|rest| rest.lines().next()).expect("logged code").trim();
    assert_eq!(code.len(), 6);
    let verified = post(&app, "/verify-email", &format!("code={code}"), &c).await;
    assert_eq!(verified.status(), StatusCode::SEE_OTHER);
    assert!(cookie(&verified).starts_with("session="));
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(1));
}

#[sqlx::test(migrations = "../migrations")]
async fn resend_budget_exhaustion_preserves_the_delivered_code(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let email = "resend-budget@example.com";
    let c = signup(&app, email).await;
    // Signup and resends share five sends per hour, including cooldown attempts.
    for _ in 0..4 {
        sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(post(&app, "/verify-email/resend", "", &c).await.status(), StatusCode::OK);
    }
    let delivered = common::code(&app, email).await;
    assert_eq!(common::mails(&app, email).await.len(), 5);
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let response = post(&app, "/verify-email/resend", "", &c).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(common::mails(&app, email).await.len(), 5, "rejection sends no email");
    assert_eq!(common::code(&app, email).await, delivered);
    assert_eq!(
        post(&app, "/verify-email", &format!("code={delivered}"), &c).await.status(),
        StatusCode::SEE_OTHER,
        "rejection must not rotate the usable code"
    );
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(1));
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(1));
}

#[sqlx::test(migrations = "../migrations")]
async fn invalid_addresses_and_passwords_do_not_consume_signup_state(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    for email in [
        ".first@example.com".to_string(),
        "last.@example.com".into(),
        "two..dots@example.com".into(),
        "a@-example.com".into(),
        "a@example-.com".into(),
        format!("a@{}.com", "x".repeat(64)),
    ] {
        assert_eq!(
            post(&app, "/sign-up", &format!("email={email}&password=correct-horse"), "").await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{email}"
        );
        assert!(common::mails(&app, &email).await.is_empty());
    }
    let password = "x".repeat(app_auth::password::MAX_LEN + 1);
    assert_eq!(
        post(&app, "/sign-up", &format!("email=oversized@example.com&password={password}"), "").await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM signup_limits").fetch_one(&pool).await.unwrap().count,
        Some(0)
    );
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
        Some(0)
    );
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
}

#[sqlx::test(migrations = "../migrations")]
async fn malformed_browser_tokens_cannot_resend_or_consume_a_valid_signup(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let email = "malformed-token@example.com";
    let valid = signup(&app, email).await;
    let code = common::code(&app, email).await;
    for (bad, resend_status, verify_status) in [
        ("short".to_string(), StatusCode::SEE_OTHER, StatusCode::SEE_OTHER),
        ("z".repeat(64), StatusCode::SEE_OTHER, StatusCode::SEE_OTHER),
        ("a".repeat(64), StatusCode::TOO_MANY_REQUESTS, StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let cookie = format!("signup={bad}");
        for (path, expected) in [("/verify-email/resend", resend_status), ("/verify-email", verify_status)] {
            let response = post(&app, path, &format!("code={code}"), &cookie).await;
            assert_eq!(response.status(), expected);
            if expected == StatusCode::SEE_OTHER {
                assert_eq!(response.headers()[header::LOCATION], "/sign-up");
            }
        }
    }
    assert_eq!(common::mails(&app, email).await.len(), 1);
    assert_eq!(sqlx::query!("SELECT attempts FROM pending_signups").fetch_one(&pool).await.unwrap().attempts, 0);
    // Invalid-length guesses must count against the same five-guess budget.
    for _ in 0..5 {
        assert_eq!(post(&app, "/verify-email", "code=x", &valid).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(common::complete(&app, &valid, email).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(0));
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&app, "/verify-email/resend", "", &valid).await.status(), StatusCode::OK);
    assert_eq!(common::complete(&app, &valid, email).await.status(), StatusCode::SEE_OTHER);
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(1));
}

#[sqlx::test(migrations = "../migrations")]
async fn account_created_during_verification_keeps_its_credentials(pool: sqlx::PgPool) {
    let (_, app) = app(pool.clone());
    let email = "concurrent-account@example.com";
    let c = signup(&app, email).await;
    let existing = app_auth::users::sign_up(&pool, email, "independent-password".into()).await.unwrap();
    assert_eq!(common::complete(&app, &c, email).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
        Some(0)
    );
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(0));
    assert!(
        sqlx::query!("SELECT kind, dedupe_key, state FROM jobs").fetch_all(&pool).await.unwrap().is_empty(),
        "no welcome or receipt job for the losing signup"
    );
    let verified =
        app_auth::users::verify_credentials(&pool, email, "independent-password".into()).await.unwrap().unwrap();
    assert_eq!(verified.id, existing.id);
    assert!(app_auth::users::verify_credentials(&pool, email, "correct-horse".into()).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../migrations")]
async fn ambiguous_mail_timeout_fails_closed_then_a_single_resend_recovers(pool: sqlx::PgPool) {
    use std::sync::{Arc, Mutex};
    #[derive(serde::Deserialize)]
    struct Mail {
        text: String,
    }
    let codes = Arc::new(Mutex::new(Vec::<String>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/messages", listener.local_addr().unwrap());
    let mail_app = axum::Router::new().route(
        "/messages",
        axum::routing::post({
            let codes = codes.clone();
            move |axum::extract::Form(mail): axum::extract::Form<Mail>| {
                let codes = codes.clone();
                async move {
                    let code = mail.text.split("code is: ").nth(1).unwrap().lines().next().unwrap().to_string();
                    let first = {
                        let mut codes = codes.lock().unwrap();
                        codes.push(code);
                        codes.len() == 1
                    };
                    // Delivery may have happened even though the provider never acknowledged it.
                    if first {
                        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    }
                    StatusCode::OK
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, mail_app).await.unwrap() });
    let (mut state, router) = app(pool.clone());
    drop(router);
    std::sync::Arc::get_mut(&mut state).unwrap().email = Some(
        app::verification::Client::with_endpoint(
            app::verification::Config {
                api_key: "test".into(),
                domain: "mg.example.com".into(),
                from: "noreply@example.com".into(),
                hash_key: "test-only-long-key-not-production-12345".into(),
            },
            endpoint,
        )
        .unwrap(),
    );
    let app = app::routes::router(state);
    // Exercise the production client's eight-second timeout, without changing its settings.
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        post(&app, "/sign-up", "email=timeout@example.com&password=correct-horse", ""),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let c = cookie(&response);
    assert_eq!(codes.lock().unwrap().len(), 1, "no automatic retry of ambiguous delivery");
    let old = codes.lock().unwrap()[0].clone();
    assert!(!sqlx::query!("SELECT delivered FROM pending_signups").fetch_one(&pool).await.unwrap().delivered);
    assert_eq!(
        post(&app, "/verify-email", &format!("code={old}"), &c).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(post(&app, "/verify-email/resend", "", &c).await.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(codes.lock().unwrap().len(), 1);
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
    sqlx::query!("UPDATE pending_signups SET last_sent_at = now() - interval '61 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&app, "/verify-email/resend", "", &c).await.status(), StatusCode::OK);
    assert_eq!(codes.lock().unwrap().len(), 2);
    let fresh = codes.lock().unwrap()[1].clone();
    assert_eq!(post(&app, "/verify-email", &format!("code={fresh}"), &c).await.status(), StatusCode::SEE_OTHER);
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(1));
    assert_eq!(sqlx::query!("SELECT count(*) AS count FROM sessions").fetch_one(&pool).await.unwrap().count, Some(1));
    server.abort();
}
