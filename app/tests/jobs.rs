//! Background jobs end to end: a verified signup enqueues the welcome email in
//! the same transaction as the account, and the worker delivers it, retrying a
//! failed delivery, without ever sending it twice.
mod common;

use lvtd_dev as app;

#[cfg(feature = "auth")]
use app::views::SITE_NAME;
#[cfg(feature = "auth")]
use app_core::Config;
#[cfg(feature = "auth")]
use app_core::jobs::{self, Role};
#[cfg(feature = "auth")]
use axum::http::header;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
#[cfg(feature = "auth")]
use std::time::Duration;
use tower::ServiceExt;

#[cfg(feature = "auth")]
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

#[cfg(feature = "auth")]
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

#[cfg(feature = "auth")]
async fn count(pool: &sqlx::PgPool, state: &str) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM jobs WHERE state = $1"#, state)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn verified_signup_queues_one_welcome_email_and_the_worker_delivers_it(pool: sqlx::PgPool) {
    let (state, app) = app(pool.clone());
    let email = "welcome@example.com";
    let response = post(&app, "/sign-up", &format!("email={email}&password=correct-horse"), "").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    // Pending signup: no account, no job yet.
    assert_eq!(count(&pool, "queued").await, 0);
    assert_eq!(common::complete(&app, &cookie, email).await.status(), StatusCode::SEE_OTHER);
    let queued = sqlx::query!("SELECT kind, dedupe_key, attempts FROM jobs").fetch_all(&pool).await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, "send_welcome_email");
    let user_id = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", email).fetch_one(&pool).await.unwrap();
    assert_eq!(queued[0].dedupe_key.as_deref(), Some(user_id.to_string().as_str()));
    assert_eq!(common::mails(&app, email).await.len(), 1, "only the verification code was sent so far");

    // The first delivery attempt fails; the worker retries and the user gets exactly one greeting.
    common::fail_next_mail(&app).await;
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let config = jobs::Config {
        role: Role::Both,
        concurrency: 1,
        poll: Duration::from_millis(50),
        shutdown: Duration::from_secs(5),
        lease: Duration::from_secs(60),
    };
    let worker = jobs::Worker::new(pool.clone(), state.clone(), app::jobs::registry(), config);
    let handle = tokio::spawn(worker.run(rx));
    tokio::time::timeout(Duration::from_secs(15), async {
        while count(&pool, "done").await != 1 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("welcome email job completes");
    shutdown.send(true).unwrap();
    handle.await.unwrap();

    let row = sqlx::query!("SELECT attempts, last_error FROM jobs").fetch_one(&pool).await.unwrap();
    assert_eq!(row.attempts, 2, "one failed delivery, one successful");
    assert!(row.last_error.is_none());
    let mails = common::mails(&app, email).await;
    assert_eq!(mails.len(), 2);
    assert_eq!(mails[0].0, format!("Verify your {SITE_NAME} email"));
    assert!(mails[0].1.starts_with(&format!("Your {SITE_NAME} verification code is:")));
    assert_eq!(mails[1].0, format!("Welcome to {SITE_NAME}"));
    assert!(mails[1].1.starts_with(&format!("Welcome to {SITE_NAME}!")));
    assert!(mails[1].1.contains("http://test.local/sign-in"));
    assert!(!mails[1].1.contains("code is"));
}

#[cfg(feature = "auth")]
#[sqlx::test(migrations = "../migrations")]
async fn jobs_without_mail_configuration_complete_as_no_ops(pool: sqlx::PgPool) {
    let (state, _app) = app::build(
        Config {
            bind_addr: "127.0.0.1:0".into(),
            site_url: "http://test.local".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        },
        pool.clone(),
    );
    let user = app_auth::users::sign_up(&pool, "quiet@example.com", "correct-horse".into()).await.unwrap();
    jobs::enqueue(&pool, &app::jobs::SendWelcomeEmail { user_id: user.id }, jobs::Enqueue::default())
        .await
        .unwrap()
        .unwrap();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let config = jobs::Config {
        role: Role::Both,
        concurrency: 1,
        poll: Duration::from_millis(50),
        shutdown: Duration::from_secs(5),
        lease: Duration::from_secs(60),
    };
    let handle = tokio::spawn(jobs::Worker::new(pool.clone(), state, app::jobs::registry(), config).run(rx));
    tokio::time::timeout(Duration::from_secs(10), async {
        while count(&pool, "done").await != 1 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    shutdown.send(true).unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn the_worker_role_serves_only_the_health_check() {
    let app = app::worker_router();
    let health = app.clone().oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(health.headers().contains_key("x-deployment-revision"));
    let home = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(home.status(), StatusCode::NOT_FOUND);
}
