//! Runtime configuration, read once from the environment at startup.
//! Every variable is documented in `.env.example`.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    /// Public origin without a trailing slash, e.g. `https://example.com`.
    pub site_url: String,
    pub database_url: String,
    /// How long a sign-in lasts without activity.
    pub session_ttl_days: i64,
}

fn opt(name: &str) -> Option<String> {
    env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind_addr: opt("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3000".into()),
            site_url: opt("SITE_URL")
                .unwrap_or_else(|| "http://localhost:3000".into())
                .trim_end_matches('/')
                .to_string(),
            database_url: opt("DATABASE_URL").ok_or_else(|| anyhow::anyhow!("DATABASE_URL is required"))?,
            session_ttl_days: opt("SESSION_TTL_DAYS").and_then(|v| v.parse().ok()).unwrap_or(30),
        })
    }

    /// Cookies are marked `Secure` whenever the site is served over https.
    pub fn secure_cookies(&self) -> bool {
        self.site_url.starts_with("https://")
    }
}
