//! Optional OTLP logs/traces and the HTTP error-reporting boundary.

use crate::SharedState;
#[cfg(feature = "posthog")]
use crate::analytics::Config;
use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
#[cfg(feature = "posthog-otlp")]
use opentelemetry::{
    propagation::{Extractor, TextMapPropagator},
    trace::TraceContextExt,
};
#[cfg(feature = "posthog-otlp")]
use opentelemetry_sdk::propagation::TraceContextPropagator;
#[cfg(feature = "sentry-errors")]
use std::sync::Arc;
use std::time::Instant;
use tracing::{Instrument, Span};
#[cfg(feature = "posthog-otlp")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

#[cfg(feature = "posthog-otlp")]
mod exporters {
    use super::Config;
    use opentelemetry::{KeyValue, trace::TracerProvider};
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_otlp::{LogExporter, Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
    use opentelemetry_sdk::{
        Resource,
        logs::SdkLoggerProvider,
        trace::{Sampler, SdkTracerProvider},
    };
    use std::{collections::HashMap, time::Duration};
    use tracing_subscriber::{EnvFilter, prelude::*};

    fn default_filter() -> String {
        format!(
            "{}=info,{}=info,{}=info,posthog_rs=warn,opentelemetry=warn",
            env!("CARGO_PKG_NAME").replace('-', "_"),
            app_core::LOG_TARGET,
            app_auth::LOG_TARGET
        )
    }

    pub struct Telemetry {
        logs: Option<SdkLoggerProvider>,
        traces: Option<SdkTracerProvider>,
    }

    impl Telemetry {
        /// Blocking HTTP exporters own runtime resources: construct/drop them off
        /// Tokio request threads. Batch processors have bounded queues and workers.
        pub async fn init(config: Option<Config>) -> anyhow::Result<Self> {
            tokio::task::spawn_blocking(move || {
                let mut telemetry = Self { logs: None, traces: None };
                let mut log_layer = None;
                let mut trace_layer = None;
                if let Some(config) = config {
                    let headers = HashMap::from([("Authorization".into(), format!("Bearer {}", config.project_token))]);
                    let resource = Resource::builder()
                        .with_service_name(env!("CARGO_PKG_NAME"))
                        .with_attributes([KeyValue::new("service.version", app_core::http::BUILD_REVISION)])
                        .build();
                    if cfg!(feature = "posthog-logs") {
                        let log_exporter = LogExporter::builder()
                            .with_http()
                            .with_endpoint(format!("{}/i/v1/logs", config.api_host))
                            .with_protocol(Protocol::HttpBinary)
                            .with_headers(headers.clone())
                            .with_timeout(Duration::from_secs(3))
                            .build()?;
                        let logs = SdkLoggerProvider::builder()
                            .with_resource(resource.clone())
                            .with_batch_exporter(log_exporter)
                            .build();
                        log_layer = Some(OpenTelemetryTracingBridge::new(&logs).with_filter(export_filter()));
                        telemetry.logs = Some(logs);
                    }
                    if cfg!(feature = "posthog-traces") {
                        let span_exporter = SpanExporter::builder()
                            .with_http()
                            .with_endpoint(format!("{}/i/v1/traces", config.api_host))
                            .with_protocol(Protocol::HttpBinary)
                            .with_headers(headers)
                            .with_timeout(Duration::from_secs(3))
                            .build()?;
                        let traces = SdkTracerProvider::builder()
                            .with_resource(resource)
                            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                                config.trace_sample_ratio,
                            ))))
                            .with_batch_exporter(span_exporter)
                            .build();
                        trace_layer = Some(
                            tracing_opentelemetry::layer()
                                .with_tracer(traces.tracer(env!("CARGO_PKG_NAME")))
                                .with_filter(export_filter()),
                        );
                        telemetry.traces = Some(traces);
                    }
                }
                let registry = tracing_subscriber::registry();
                #[cfg(feature = "sentry-layer")]
                let registry = registry.with(crate::sentry::tracing_layer());
                registry
                    .with(trace_layer)
                    .with(log_layer)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .compact()
                            .with_target(false)
                            .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter().into())),
                    )
                    .try_init()?;
                Ok(telemetry)
            })
            .await?
        }

        pub async fn shutdown(self) {
            // Keep exporter failures visible on stderr; never feed them into OTLP.
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(traces) = self.traces
                    && let Err(error) = traces.shutdown_with_timeout(Duration::from_secs(5))
                {
                    eprintln!("trace shutdown failed: {error}");
                }
                if let Some(logs) = self.logs
                    && let Err(error) = logs.shutdown_with_timeout(Duration::from_secs(5))
                {
                    eprintln!("log shutdown failed: {error}");
                }
            })
            .await;
        }
    }

    fn export_filter() -> EnvFilter {
        // An allowlist prevents transport/SDK failures recursively exporting
        // themselves, and excludes SQL statements and third-party debug output.
        EnvFilter::new(format!(
            "off,{}=info,{}=info,{}=info",
            env!("CARGO_PKG_NAME").replace('-', "_"),
            app_core::LOG_TARGET,
            app_auth::LOG_TARGET
        ))
    }
}
#[cfg(feature = "posthog-otlp")]
pub use exporters::Telemetry;

#[cfg(not(feature = "posthog-otlp"))]
pub struct Telemetry;
#[cfg(not(feature = "posthog-otlp"))]
impl Telemetry {
    pub async fn init(#[cfg(feature = "posthog")] _config: Option<Config>) -> anyhow::Result<Self> {
        #[cfg(feature = "sentry-layer")]
        {
            use tracing_subscriber::{EnvFilter, prelude::*};
            tracing_subscriber::registry()
                .with(crate::sentry::tracing_layer())
                .with(tracing_subscriber::fmt::layer().compact().with_target(false).with_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                        format!(
                            "{}=info,{}=info,{}=info",
                            env!("CARGO_PKG_NAME").replace('-', "_"),
                            app_core::LOG_TARGET,
                            app_auth::LOG_TARGET
                        )
                        .into()
                    }),
                ))
                .try_init()?;
        }
        #[cfg(not(feature = "sentry-layer"))]
        app_core::telemetry::init(&format!(
            "{}=info,{}=info,{}=info",
            env!("CARGO_PKG_NAME").replace('-', "_"),
            app_core::LOG_TARGET,
            app_auth::LOG_TARGET
        ));
        Ok(Self)
    }
    pub async fn shutdown(self) {}
}

#[cfg(feature = "posthog-otlp")]
pub fn trace_ids() -> Option<(String, String)> {
    let context = Span::current().context();
    let span = context.span();
    let context = span.span_context();
    context.is_valid().then(|| (context.trace_id().to_string(), context.span_id().to_string()))
}

#[cfg(feature = "posthog-otlp")]
struct Headers<'a>(&'a axum::http::HeaderMap);
#[cfg(feature = "posthog-otlp")]
impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|key| key.as_str()).collect()
    }
}

#[cfg(not(feature = "posthog-otlp"))]
pub fn trace_ids() -> Option<(String, String)> {
    None
}

pub async fn request(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let enabled = false;
    #[cfg(feature = "posthog")]
    let enabled = enabled || state.analytics.is_some();
    #[cfg(feature = "sentry-server")]
    let enabled = enabled || state.sentry.is_some();
    if !enabled {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path == "/healthz" || path.starts_with("/assets/") {
        return next.run(req).await;
    }
    // Route templates bound cardinality and omit query strings, IDs and input.
    let route = req.extensions().get::<MatchedPath>().map_or("unmatched", |path| path.as_str()).to_owned();
    let method = req.method().to_string();
    let span = tracing::info_span!("http.request", otel.name = %format!("{method} {route}"), otel.kind = "server",
        http.request.method = %method, http.route = %route, http.response.status_code = tracing::field::Empty,
        posthog.distinct_id = tracing::field::Empty, otel.status_code = tracing::field::Empty);
    #[cfg(feature = "posthog-otlp")]
    let parent = TraceContextPropagator::new().extract(&Headers(req.headers()));
    #[cfg(feature = "posthog-otlp")]
    if parent.span().span_context().is_valid() {
        let _ = span.set_parent(parent);
    }
    let response = async move {
        let started = Instant::now();
        let response = next.run(req).await;
        #[cfg(feature = "sentry-errors")]
        let mut response = response;
        let status = response.status();
        Span::current().record("http.response.status_code", status.as_u16());
        #[cfg(feature = "sentry-errors")]
        let error = response.extensions_mut().remove::<Arc<anyhow::Error>>();
        if status.is_server_error() {
            Span::current().record("otel.status_code", "ERROR");
            #[cfg(feature = "sentry-errors")]
            if state.sentry.is_some() {
                let fallback = anyhow::anyhow!("HTTP {} on {} {}", status.as_u16(), method, route);
                sentry::integrations::anyhow::capture_anyhow(error.as_deref().unwrap_or(&fallback));
            }
        }
        tracing::info!(http.route = %route, http.response.status_code = status.as_u16(), duration_ms = started.elapsed().as_millis() as u64, "request completed");
        response
    }.instrument(span);
    #[cfg(feature = "posthog")]
    let response = crate::analytics::REQUEST_USER_ID.scope(std::cell::RefCell::new(None), response);
    response.await
}
