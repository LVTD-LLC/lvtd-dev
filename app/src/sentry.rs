//! Optional Sentry configuration and request isolation. No request bodies,
//! cookies, headers, query strings or user profiles are attached to Sentry.
#[cfg(feature = "sentry-server")]
use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
#[cfg(feature = "sentry-server")]
use sentry::SentryFutureExt;
#[cfg(feature = "sentry-metrics")]
use std::time::Instant;
#[cfg(feature = "sentry-server")]
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub dsn: String,
    pub environment: String,
    pub traces_sample_rate: f32,
    pub replay_sample_rate: f32,
    pub profile_sample_rate: f32,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::parse(|key| std::env::var(key).ok())
    }

    pub fn parse(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let dsn = get("SENTRY_DSN").unwrap_or_default().trim().to_owned();
        if dsn.is_empty() {
            return Ok(None);
        }
        dsn.parse::<sentry_types::Dsn>().map_err(|_| anyhow::anyhow!("invalid SENTRY_DSN"))?;
        let rate = |key, default, enabled: bool| -> anyhow::Result<f32> {
            if !enabled {
                return Ok(0.0);
            }
            let value = get(key).map(|value| value.parse::<f32>()).transpose()?.unwrap_or(default);
            anyhow::ensure!(value.is_finite() && (0.0..=1.0).contains(&value), "{key} must be between 0 and 1");
            Ok(value)
        };
        Ok(Some(Self {
            dsn,
            environment: get("SENTRY_ENVIRONMENT").unwrap_or_else(|| "development".into()),
            traces_sample_rate: rate("SENTRY_TRACES_SAMPLE_RATE", 1.0, cfg!(feature = "sentry-traces"))?,
            replay_sample_rate: rate("SENTRY_REPLAY_SAMPLE_RATE", 0.25, cfg!(feature = "sentry-replay"))?,
            profile_sample_rate: rate("SENTRY_PROFILE_SAMPLE_RATE", 0.25, cfg!(feature = "sentry-traces"))?,
        }))
    }

    #[cfg(feature = "sentry-server")]
    pub fn client_options(&self) -> sentry::ClientOptions {
        sentry::ClientOptions::new()
            .dsn(&self.dsn)
            .sample_rate(if cfg!(feature = "sentry-errors") { 1.0 } else { 0.0 })
            .release(release())
            .environment(self.environment.clone())
            .traces_sample_rate(self.traces_sample_rate)
            .send_default_pii(false)
            .shutdown_timeout(Duration::from_secs(5))
    }
}

pub fn release() -> &'static str {
    static RELEASE: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| format!("{}@{}", env!("CARGO_PKG_NAME"), app_core::http::BUILD_REVISION));
    &RELEASE
}

pub fn trace_headers() -> Vec<(&'static str, String)> {
    #[cfg(feature = "sentry-traces")]
    return sentry::configure_scope(|scope| scope.iter_trace_propagation_headers().collect());
    #[cfg(not(feature = "sentry-traces"))]
    Vec::new()
}

/// Bind the hub on every poll, never hold a thread-local scope across an await.
#[cfg(feature = "sentry-server")]
pub async fn request(State(state): State<crate::SharedState>, req: Request, next: Next) -> Response {
    if state.sentry.is_none() || req.uri().path() == "/healthz" || req.uri().path().starts_with("/assets/") {
        return next.run(req).await;
    }
    let hub = Arc::new(sentry::Hub::new_from_top(sentry::Hub::current()));
    async move {
        let route = req.extensions().get::<MatchedPath>().map_or("unmatched", |p| p.as_str()).to_owned();
        #[cfg(any(feature = "sentry-traces", feature = "sentry-metrics"))]
        let method = req.method().to_string();
        sentry::configure_scope(|scope| scope.set_tag("http.route", &route));
        #[cfg(feature = "sentry-traces")]
        let transaction = {
            let name = format!("{method} {route}");
            let context = sentry::TransactionContext::continue_from_headers(
                &name,
                "http.server",
                req.headers().iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str(), v))),
            );
            let transaction = sentry::start_transaction(context);
            transaction.set_data("http.request.method", method.clone().into());
            transaction.set_data("http.route", route.clone().into());
            sentry::configure_scope(|scope| scope.set_span(Some(transaction.clone().into())));
            transaction
        };
        #[cfg(feature = "sentry-metrics")]
        let started = Instant::now();
        let response = next.run(req).await;
        #[cfg(feature = "sentry-traces")]
        let mut response = response;
        #[cfg(any(feature = "sentry-traces", feature = "sentry-metrics"))]
        let status = response.status().as_u16();
        #[cfg(feature = "sentry-traces")]
        {
            transaction.set_data("http.response.status_code", status.into());
            transaction.set_status(match status {
                500..=599 => sentry::protocol::SpanStatus::InternalError,
                404 => sentry::protocol::SpanStatus::NotFound,
                401 => sentry::protocol::SpanStatus::Unauthenticated,
                403 => sentry::protocol::SpanStatus::PermissionDenied,
                400..=499 => sentry::protocol::SpanStatus::InvalidArgument,
                _ => sentry::protocol::SpanStatus::Ok,
            });
        }
        #[cfg(feature = "sentry-metrics")]
        {
            sentry::metrics::counter("http.server.requests", 1)
                .attribute("http.route", route.clone())
                .attribute("http.request.method", method.clone())
                .attribute("http.response.status_code", status)
                .capture();
            sentry::metrics::distribution("http.server.duration", started.elapsed().as_secs_f64() * 1000.0)
                .unit(sentry::protocol::Unit::Millisecond)
                .attribute("http.route", route)
                .attribute("http.request.method", method)
                .attribute("http.response.status_code", status)
                .capture();
        }
        #[cfg(feature = "sentry-traces")]
        if state.sentry.as_ref().is_some_and(|config| config.profile_sample_rate > 0.0)
            && response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .is_some_and(|v| v.as_bytes().starts_with(b"text/html"))
        {
            response.headers_mut().insert("document-policy", axum::http::HeaderValue::from_static("js-profiling"));
        }
        #[cfg(feature = "sentry-traces")]
        transaction.finish();
        response
    }
    .bind_hub(hub)
    .await
}

/// Export application logs without duplicating errors captured at the boundary.
#[cfg(feature = "sentry-layer")]
pub fn tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer;
    sentry::integrations::tracing::layer()
        .event_filter(|_| {
            #[cfg(feature = "sentry-logs")]
            return sentry::integrations::tracing::EventFilter::Log;
            #[cfg(not(feature = "sentry-logs"))]
            sentry::integrations::tracing::EventFilter::Ignore
        })
        .span_filter(|metadata| cfg!(feature = "sentry-traces") && metadata.name() != "http.request")
        .with_filter(tracing_subscriber::EnvFilter::new(format!(
            "off,{}=info,{}=info,{}=info",
            env!("CARGO_PKG_NAME").replace('-', "_"),
            app_core::LOG_TARGET,
            app_auth::LOG_TARGET
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_config_validates_dsn_and_rates() {
        assert!(Config::parse(|_| None).unwrap().is_none());
        assert!(Config::parse(|key| (key == "SENTRY_DSN").then(|| "bad".into())).is_err());
        #[cfg(feature = "sentry-traces")]
        for value in ["NaN", "inf", "-0.1", "1.1"] {
            assert!(
                Config::parse(|key| match key {
                    "SENTRY_DSN" => Some("https://public@example.com/1".into()),
                    "SENTRY_TRACES_SAMPLE_RATE" => Some(value.into()),
                    _ => None,
                })
                .is_err()
            );
        }
        let cfg =
            Config::parse(|key| (key == "SENTRY_DSN").then(|| "https://public@example.com/1".into())).unwrap().unwrap();
        assert_eq!(cfg.traces_sample_rate, if cfg!(feature = "sentry-traces") { 1.0 } else { 0.0 });
        assert_eq!(cfg.replay_sample_rate, if cfg!(feature = "sentry-replay") { 0.25 } else { 0.0 });
    }
}
