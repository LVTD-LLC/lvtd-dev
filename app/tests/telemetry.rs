#![cfg(all(feature = "posthog", feature = "auth"))]
//! A real HTTP collector verifies the SDK and OTLP wire payloads without sending
//! test accounts or errors to PostHog. One test owns the process-wide subscriber.
mod common;
use lvtd_dev as app;

use app::{analytics, observability::Telemetry};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    middleware,
    routing::get,
};
use http_body_util::BodyExt;
use opentelemetry_proto::tonic::collector::{logs::v1::ExportLogsServiceRequest, trace::v1::ExportTraceServiceRequest};
use prost::Message;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct Payload {
    path: String,
    authorization: String,
    bytes: Vec<u8>,
}

async fn send(app: &Router, method: &str, path: &str, body: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ORIGIN, "http://test.local")
                .header(header::COOKIE, cookie)
                .header("traceparent", "00-11111111111111111111111111111111-2222222222222222-01")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../migrations")]
async fn exports_events_profiles_errors_logs_and_correlated_traces(pool: sqlx::PgPool) {
    let payloads = Arc::new(Mutex::new(Vec::<Payload>::new()));
    let received = payloads.clone();
    let collector = Router::new().fallback(move |req: Request<Body>| {
        let received = received.clone();
        async move {
            let path = req.uri().path().to_owned();
            let authorization =
                req.headers().get(header::AUTHORIZATION).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
            let bytes = req.into_body().collect().await.unwrap().to_bytes().to_vec();
            received.lock().unwrap().push(Payload { path, authorization, bytes });
            StatusCode::OK
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = analytics::Config::parse("phc_test", "").unwrap().unwrap();
    config.api_host = format!("http://{}", listener.local_addr().unwrap());
    config.trace_sample_ratio = 1.0;
    let server = tokio::spawn(async { axum::serve(listener, collector).await.unwrap() });
    let telemetry = Telemetry::init(Some(config.clone())).await.unwrap();
    #[cfg(feature = "posthog-server")]
    posthog_rs::init_global(config.client_options().unwrap()).await.unwrap();
    let cfg = app_core::Config {
        bind_addr: "127.0.0.1:0".into(),
        site_url: "http://test.local".into(),
        database_url: String::new(),
        session_ttl_days: 30,
    };
    let (state, app) = common::mail_app(app::build_with_analytics(cfg, pool, Some(config)));
    let credentials = "email=telemetry%40example.com&password=never-export-this-password";
    let signup = send(&app, "POST", "/sign-up", credentials, "").await;
    assert_eq!(signup.status(), StatusCode::SEE_OTHER);
    let cookie = signup.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    assert_eq!(send(&app, "POST", "/sign-up", credentials, "").await.status(), StatusCode::TOO_MANY_REQUESTS);
    let signup_cookie = cookie.clone();
    let verification_code = common::code(&app, "telemetry@example.com").await;
    let verified = common::complete(&app, &cookie, "telemetry@example.com").await;
    assert_eq!(verified.status(), StatusCode::SEE_OTHER);
    let cookie = verified.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    assert_eq!(
        send(&app, "POST", "/sign-in", "email=telemetry%40example.com&password=wrong", "").await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(send(&app, "POST", "/sign-in", credentials, "").await.status(), StatusCode::SEE_OTHER);
    for path in ["/healthz", "/assets/style.css"] {
        assert_eq!(send(&app, "GET", path, "", "").await.status(), StatusCode::OK);
    }
    // The actual site error type preserves source errors, while HTTP responses
    // stay generic. An authenticated synthetic route exercises that boundary.
    let failure = Router::new()
        .route(
            "/failure",
            get(|app_auth::CurrentUser(user): app_auth::CurrentUser| async move {
                analytics::record_user(&user);
                Err::<(), app::AppError>(
                    anyhow::Error::new(std::io::Error::other("synthetic storage failure"))
                        .context("loading test project")
                        .into(),
                )
            }),
        )
        .route(
            "/anonymous-failure",
            get(|| async { Err::<(), app::AppError>(std::io::Error::other("synthetic anonymous failure").into()) }),
        )
        .layer(middleware::from_fn_with_state(state.clone(), app::observability::request))
        .with_state(state);
    let failed = send(&failure, "GET", "/failure?secret=never-export-this-query", "", &cookie).await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(failed.into_body().collect().await.unwrap().to_bytes(), "internal error");
    assert_eq!(send(&app, "POST", "/sign-out", "", &cookie).await.status(), StatusCode::SEE_OTHER);
    assert_eq!(send(&failure, "GET", "/anonymous-failure", "", "").await.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // An unknown/anonymous request must not inherit the previous request's user.
    assert_eq!(send(&app, "GET", "/missing", "", "").await.status(), StatusCode::NOT_FOUND);
    #[cfg(feature = "posthog-server")]
    tokio::task::spawn_blocking(|| {
        assert!(std::panic::catch_unwind(|| panic!("synthetic telemetry panic")).is_err());
    })
    .await
    .unwrap();
    #[cfg(feature = "posthog-server")]
    posthog_rs::shutdown().await;
    telemetry.shutdown().await;
    server.abort();
    let payloads = payloads.lock().unwrap();
    let mut events = Vec::<Value>::new();
    let mut logs = Vec::new();
    let mut spans = Vec::new();
    for payload in payloads.iter() {
        let wire = String::from_utf8_lossy(&payload.bytes);
        for forbidden in [
            "never-export-this-password",
            "never-export-this-query",
            "Private Project Name",
            "private-description",
            "private-slug",
            &cookie,
            &signup_cookie,
            &verification_code,
        ] {
            assert!(!wire.contains(forbidden), "export leaked {forbidden}");
        }
        match payload.path.as_str() {
            "/batch/" => events.extend(
                serde_json::from_slice::<Value>(&payload.bytes).unwrap()["batch"].as_array().unwrap().iter().cloned(),
            ),
            "/i/v1/logs" => {
                assert_eq!(payload.authorization, "Bearer phc_test");
                let export = ExportLogsServiceRequest::decode(payload.bytes.as_slice()).unwrap();
                for resource in export.resource_logs {
                    assert!(format!("{:?}", resource.resource).contains("lvtd-dev"));
                    logs.extend(resource.scope_logs.into_iter().flat_map(|scope| scope.log_records));
                }
            }
            "/i/v1/traces" => {
                assert_eq!(payload.authorization, "Bearer phc_test");
                let export = ExportTraceServiceRequest::decode(payload.bytes.as_slice()).unwrap();
                for resource in export.resource_spans {
                    assert!(format!("{:?}", resource.resource).contains("lvtd-dev"));
                    spans.extend(resource.scope_spans.into_iter().flat_map(|scope| scope.spans));
                }
            }
            path => panic!("unexpected export path {path}"),
        }
    }
    for name in ["user signed up", "user signed in", "user signed out"] {
        assert_eq!(
            events.iter().filter(|event| event["event"] == name).count(),
            usize::from(cfg!(feature = "posthog-product")),
            "{name}: {events:?}"
        );
    }
    assert!(
        !events.iter().any(|event| event["event"] == "$exception"),
        "PostHog error tracking is not used: {events:?}"
    );
    for event in &events {
        assert_eq!(event["properties"]["$set"]["email"], "telemetry@example.com");
        assert!(event["properties"]["$set_once"]["created_at"].as_str().is_some());
        if cfg!(feature = "posthog-traces") {
            assert_eq!(event["properties"]["$trace_id"], "11111111111111111111111111111111");
        }
    }
    assert_eq!(!spans.is_empty(), cfg!(feature = "posthog-traces"));
    assert_eq!(!logs.is_empty(), cfg!(feature = "posthog-logs"));
    if cfg!(feature = "posthog-traces") {
        let failed_span = spans.iter().find(|span| span.name == "GET /failure").expect("HTTP server span exported");
        assert_eq!(failed_span.trace_id, vec![0x11; 16]);
        assert_eq!(failed_span.parent_span_id, vec![0x22; 8]);
        assert_eq!(failed_span.status.as_ref().unwrap().code, 2);
        if cfg!(feature = "posthog-logs") {
            let failed_log = logs.iter().find(|log| format!("{:?}", log.body).contains("request failed")).unwrap();
            assert_eq!(failed_log.trace_id, failed_span.trace_id);
            assert_eq!(failed_log.span_id, failed_span.span_id);
        }
    }
    assert!(!spans.iter().any(|span| span.name.contains("healthz") || span.name.contains("assets")));
}
