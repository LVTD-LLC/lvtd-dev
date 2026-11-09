//! Optional PostHog configuration, read once at startup.

#[derive(Clone, Debug)]
pub struct Config {
    pub project_token: String,
    pub api_host: String,
    pub sdk_url: String,
    pub trace_sample_ratio: f64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let mut config = Self::parse(
            &std::env::var("POSTHOG_PROJECT_TOKEN").unwrap_or_default(),
            &std::env::var("POSTHOG_HOST").unwrap_or_default(),
        )?;
        if let Some(config) = &mut config {
            config.trace_sample_ratio =
                parse_sample_ratio(&std::env::var("POSTHOG_TRACE_SAMPLE_RATIO").unwrap_or_else(|_| "0.1".into()))?;
        }
        Ok(config)
    }

    pub fn parse(project_token: &str, host: &str) -> anyhow::Result<Option<Self>> {
        let project_token = project_token.trim();
        if project_token.is_empty() {
            return Ok(None);
        }
        let host = host.trim().trim_end_matches('/');
        let (api_host, sdk_url) = match host {
            "" | "https://us.i.posthog.com" => {
                ("https://us.i.posthog.com", "https://us-assets.i.posthog.com/static/array.js")
            }
            "https://eu.i.posthog.com" => {
                ("https://eu.i.posthog.com", "https://eu-assets.i.posthog.com/static/array.js")
            }
            _ => anyhow::bail!("POSTHOG_HOST must be https://us.i.posthog.com or https://eu.i.posthog.com"),
        };
        Ok(Some(Self {
            project_token: project_token.into(),
            api_host: api_host.into(),
            sdk_url: sdk_url.into(),
            trace_sample_ratio: 0.1,
        }))
    }

    #[cfg(feature = "posthog-server")]
    pub fn client_options(&self) -> anyhow::Result<posthog_rs::ClientOptions> {
        Ok(posthog_rs::ClientOptionsBuilder::default()
            .api_key(self.project_token.clone())
            .host(&self.api_host)
            .max_queue_size(1000)
            .max_batch_size(100)
            .request_timeout_seconds(3)
            .shutdown_timeout_ms(5000)
            .before_send(|mut event| {
                event.insert_prop("service", env!("CARGO_PKG_NAME")).ok()?;
                event.insert_prop("deployment_revision", app_core::http::BUILD_REVISION).ok()?;
                Some(event)
            })
            .build()?)
    }
}

fn parse_sample_ratio(value: &str) -> anyhow::Result<f64> {
    let ratio: f64 = value.trim().parse()?;
    anyhow::ensure!(
        ratio.is_finite() && (0.0..=1.0).contains(&ratio),
        "POSTHOG_TRACE_SAMPLE_RATIO must be between 0 and 1"
    );
    Ok(ratio)
}

#[cfg(feature = "posthog-product")]
impl crate::AppState {
    /// Known, successful business actions only. No network I/O on the request path.
    pub fn track(&self, user: &app_auth::User, name: &str, properties: serde_json::Value) {
        if self.analytics.is_none() {
            return;
        }
        record_user(user);
        let mut event = posthog_rs::Event::new(name.to_owned(), user.id.to_string());
        if let Some(properties) = properties.as_object() {
            for (key, value) in properties {
                event.insert_prop(key, value).expect("JSON values serialize");
            }
        }
        event.insert_prop("$set", serde_json::json!({"email": user.email})).expect("JSON values serialize");
        event.insert_prop("$set_once", serde_json::json!({"created_at": user.created_at.format(&time::format_description::well_known::Rfc3339).ok()})).expect("JSON values serialize");
        if let Some((trace_id, span_id)) = crate::observability::trace_ids() {
            event.insert_prop("$trace_id", trace_id).expect("string serializes");
            event.insert_prop("$span_id", span_id).expect("string serializes");
        }
        posthog_rs::capture(event);
        tracing::info!(posthogDistinctId = %user.id, action = name, "business action succeeded");
    }
}

tokio::task_local! {
    pub(crate) static REQUEST_USER_ID: std::cell::RefCell<Option<String>>;
}

pub fn record_user(user: &app_auth::User) {
    let id = user.id.to_string();
    tracing::Span::current().record("posthog.distinct_id", &id);
    let _ = REQUEST_USER_ID.try_with(|current| *current.borrow_mut() = Some(id));
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn trace_sample_ratio_is_bounded() {
        for value in ["0", "0.1", "1"] {
            assert!(super::parse_sample_ratio(value).is_ok());
        }
        for value in ["NaN", "inf", "-0.1", "1.1", "invalid", ""] {
            assert!(super::parse_sample_ratio(value).is_err());
        }
    }

    #[test]
    fn optional_config_selects_matching_cloud_hosts() {
        assert!(Config::parse("  ", "").unwrap().is_none());
        let us = Config::parse(" phc_test ", "").unwrap().unwrap();
        assert_eq!(us.project_token, "phc_test");
        assert_eq!(us.api_host, "https://us.i.posthog.com");
        assert_eq!(us.sdk_url, "https://us-assets.i.posthog.com/static/array.js");
        let eu = Config::parse("phc_test", " https://eu.i.posthog.com/ ").unwrap().unwrap();
        assert_eq!(eu.api_host, "https://eu.i.posthog.com");
        assert_eq!(eu.sdk_url, "https://eu-assets.i.posthog.com/static/array.js");
        for host in ["https://us.posthog.com", "http://us.i.posthog.com", "javascript:alert(1)"] {
            assert!(Config::parse("phc_test", host).is_err());
        }
    }
}
