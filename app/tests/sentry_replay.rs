#![cfg(all(feature = "sentry-replay", not(feature = "sentry-server")))]
use lvtd_dev as app;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn replay_runs_without_server_instrumentation() {
    let config = app::sentry::Config::parse(|key| (key == "SENTRY_DSN").then(|| "https://public@example.com/1".into()))
        .unwrap()
        .unwrap();
    let cfg = app_core::Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://unused@localhost/unused").unwrap();
    let (_, app) = app::build_configured(
        cfg,
        pool,
        #[cfg(feature = "posthog")]
        None,
        Some(config),
        #[cfg(feature = "stripe")]
        None,
        #[cfg(feature = "storage")]
        app::scratch_storage(),
        #[cfg(feature = "qdrant")]
        None,
    );
    let response = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
    assert!(!response.headers().contains_key("document-policy"));
    let html = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&html);
    assert!(html.contains("data-replay=\"true\""));
    assert!(!html.contains("name=\"sentry-trace\""));
    for key in ["errors", "traces", "logs", "metrics"] {
        assert!(html.contains(&format!("data-{key}=\"false\"")));
    }
}
