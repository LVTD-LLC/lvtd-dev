#![cfg(feature = "sentry-server")]
//! Exercise the real SDK pipeline with a memory transport; no external exports.
use lvtd_dev as app;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use http_body_util::BodyExt;
use sentry::protocol::EnvelopeItem;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Default)]
struct Collector(Mutex<Vec<sentry::Envelope>>);
impl sentry::Transport for Collector {
    fn send_envelope(&self, envelope: sentry::Envelope) {
        self.0.lock().unwrap().push(envelope);
    }
}

#[test]
fn exports_correlated_errors_logs_metrics_and_isolates_concurrent_requests() {
    let collector = Arc::new(Collector::default());
    let config = app::sentry::Config::parse(|key| (key == "SENTRY_DSN").then(|| "https://public@example.com/1".into()))
        .unwrap()
        .unwrap();
    let guard = sentry::init(config.client_options().transport(collector.clone()));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        #[cfg(feature = "posthog")]
        let analytics = None;
        let telemetry = app::observability::Telemetry::init(
            #[cfg(feature = "posthog")]
            analytics.clone(),
        )
        .await
        .unwrap();
        let cfg = app_core::Config {
            bind_addr: "127.0.0.1:0".into(),
            site_url: "http://test.local".into(),
            database_url: String::new(),
            session_ttl_days: 30,
        };
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://unused@localhost/unused").unwrap();
        let (state, site) = app::build_configured(
            cfg.clone(),
            pool.clone(),
            #[cfg(feature = "posthog")]
            analytics,
            Some(config),
            #[cfg(feature = "stripe")]
            None,
            #[cfg(feature = "storage")]
            app::scratch_storage(),
            #[cfg(feature = "qdrant")]
            None,
        );
        let response = site.clone().oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.headers().contains_key("document-policy"), cfg!(feature = "sentry-traces"));
        let html = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&html).contains("/assets/sentry.js?v="));
        assert_eq!(String::from_utf8_lossy(&html).contains("name=\"sentry-trace\""), cfg!(feature = "sentry-traces"));
        for (key, enabled) in [
            ("errors", cfg!(feature = "sentry-errors")),
            ("traces", cfg!(feature = "sentry-traces")),
            ("logs", cfg!(feature = "sentry-logs")),
            ("metrics", cfg!(feature = "sentry-metrics")),
            ("replay", cfg!(feature = "sentry-replay")),
        ] {
            assert!(String::from_utf8_lossy(&html).contains(&format!("data-{key}=\"{enabled}\"")));
        }
        let (_, disabled) = app::build(cfg, pool);
        let response = disabled.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert!(!response.headers().contains_key("document-policy"));
        assert!(
            !String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes()).contains("sentry.js")
        );
        for path in ["/healthz", "/assets/sentry.js"] {
            assert_eq!(
                site.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap().status(),
                StatusCode::OK
            );
        }
        let failure = Router::new()
            .route(
                "/first",
                get(|| async {
                    sentry::configure_scope(|scope| scope.set_tag("isolation", "first"));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    Err::<(), app::AppError>(
                        anyhow::anyhow!("synthetic first failure").context("loading test object").into(),
                    )
                }),
            )
            .route(
                "/second",
                get(|| async {
                    tokio::task::yield_now().await;
                    Err::<(), app::AppError>(anyhow::anyhow!("synthetic second failure").into())
                }),
            )
            .layer(middleware::from_fn_with_state(state.clone(), app::observability::request))
            .layer(middleware::from_fn_with_state(state, app::sentry::request));
        let request = |path, trace| {
            Request::get(path)
                .header("sentry-trace", trace)
                .header("cookie", "never-export-cookie")
                .body(Body::from("never-export-body"))
                .unwrap()
        };
        let (first, second) = tokio::join!(
            failure.clone().oneshot(request(
                "/first?secret=never-export-query",
                "11111111111111111111111111111111-2222222222222222-1"
            )),
            failure.oneshot(request("/second", "33333333333333333333333333333333-4444444444444444-1")),
        );
        assert_eq!(first.unwrap().status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(second.unwrap().status(), StatusCode::INTERNAL_SERVER_ERROR);
        telemetry.shutdown().await;
    });
    assert!(guard.flush(Some(std::time::Duration::from_secs(5))));
    let envelopes = collector.0.lock().unwrap();
    let mut events = Vec::new();
    let mut transactions = Vec::new();
    let mut wire = Vec::new();
    for envelope in envelopes.iter() {
        envelope.to_writer(&mut wire).unwrap();
        for item in envelope.items() {
            match item {
                EnvelopeItem::Event(event) => events.push(serde_json::to_value(event).unwrap()),
                EnvelopeItem::Transaction(transaction) => transactions.push(serde_json::to_value(transaction).unwrap()),
                _ => {}
            }
        }
    }
    let wire = String::from_utf8(wire).unwrap();
    for forbidden in ["never-export-cookie", "never-export-body", "never-export-query"] {
        assert!(!wire.contains(forbidden));
    }
    assert_eq!(events.len(), if cfg!(feature = "sentry-errors") { 2 } else { 0 }, "only selected errors exported");
    if cfg!(feature = "sentry-errors") {
        let first = events.iter().find(|e| e["tags"]["http.route"] == "/first").unwrap();
        let second = events.iter().find(|e| e["tags"]["http.route"] == "/second").unwrap();
        assert_eq!(first["tags"]["isolation"], "first");
        assert!(second["tags"].get("isolation").is_none());
        assert!(first.to_string().contains("loading test object"));
        if cfg!(feature = "sentry-traces") {
            assert_eq!(first["contexts"]["trace"]["trace_id"], "11111111111111111111111111111111");
            assert_eq!(second["contexts"]["trace"]["trace_id"], "33333333333333333333333333333333");
        }
    }
    assert_eq!(!transactions.is_empty(), cfg!(feature = "sentry-traces"));
    if cfg!(feature = "sentry-traces") {
        let transaction = transactions.iter().find(|t| t["transaction"] == "GET /first").unwrap();
        assert_eq!(transaction["contexts"]["trace"]["status"], "internal_error");
        assert_eq!(transaction["contexts"]["trace"]["trace_id"], "11111111111111111111111111111111");
        assert!(transactions.iter().all(|t| !t.to_string().contains("healthz") && !t.to_string().contains("/assets/")));
    }
    assert_eq!(wire.contains("\"type\":\"log\""), cfg!(feature = "sentry-logs"));
    assert_eq!(wire.contains("request completed"), cfg!(feature = "sentry-logs"));
    assert_eq!(wire.contains("http.server.requests"), cfg!(feature = "sentry-metrics"));
    assert_eq!(wire.contains("http.server.duration"), cfg!(feature = "sentry-metrics"));
}
