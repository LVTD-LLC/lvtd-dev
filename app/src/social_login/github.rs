//! GitHub as an [`social_login::Client`](super::Client): an OAuth App's
//! authorization code flow with S256 PKCE. GitHub issues no ID token, so the
//! identity comes from the REST API: the numeric user id (never the login,
//! which users can rename) and the primary verified email. The access token is
//! used for those two reads and then revoked. Secrets and provider responses
//! never enter error messages.
use super::{Rejected, bounded_json};
use app_auth::webauthn::b64url_encode;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}
impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::parse(
            std::env::var("GITHUB_CLIENT_ID").unwrap_or_default(),
            std::env::var("GITHUB_CLIENT_SECRET").unwrap_or_default(),
        )
    }
    fn parse(client_id: String, client_secret: String) -> anyhow::Result<Option<Self>> {
        if client_id.is_empty() && client_secret.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            !client_id.trim().is_empty() && !client_secret.trim().is_empty(),
            "Set GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET together"
        );
        anyhow::ensure!(
            client_id.len() <= 64 && client_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'),
            "Invalid GITHUB_CLIENT_ID"
        );
        Ok(Some(Self { client_id, client_secret }))
    }
}

pub struct Client {
    config: Config,
    http: reqwest::Client,
    redirect_uri: String,
    token_url: String,
    api_url: String,
}

/// The parts of `GET /user/emails` this module reads.
#[derive(Deserialize)]
pub(super) struct Email {
    pub email: String,
    pub primary: bool,
    pub verified: bool,
}

/// GitHub's verified primary address, if usable for sign-up. Noreply aliases
/// are not deliverable; anything else unusual is rejected rather than guessed.
pub(super) fn signup_email(emails: &[Email]) -> Option<String> {
    let primary = emails.iter().find(|e| e.primary && e.verified)?;
    let email = app_auth::users::normalize_email(&primary.email)?;
    (email.matches('@').count() == 1
        && !email.ends_with("@users.noreply.github.com")
        && !email.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then_some(email)
}

impl Client {
    pub fn new(config: Config, site_url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                // GitHub's REST API rejects requests without a User-Agent.
                .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
                .build()?,
            redirect_uri: super::redirect_uri(site_url, "github")?,
            token_url: "https://github.com/login/oauth/access_token".into(),
            api_url: "https://api.github.com".into(),
        })
    }

    async fn exchange(&self, code: &str, verifier: &str) -> Result<super::Identity, Rejected> {
        // GitHub reports failures as 200 responses with an `error` field.
        #[derive(Deserialize)]
        struct Token {
            access_token: Option<String>,
        }
        let response = self
            .http
            .post(&self.token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", &self.config.client_secret),
                ("code", code),
                ("redirect_uri", &self.redirect_uri),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| Rejected)?;
        let token: Token = bounded_json(response).await?;
        let token = token.access_token.filter(|t| !t.is_empty() && t.len() <= 512).ok_or(Rejected)?;
        let identity = self.identity(&token).await;
        self.revoke(&token).await;
        identity
    }

    async fn identity(&self, token: &str) -> Result<super::Identity, Rejected> {
        #[derive(Deserialize)]
        struct User {
            id: u64,
        }
        let user: User = bounded_json(self.api(&format!("{}/user", self.api_url), token).await?).await?;
        let emails: Vec<Email> =
            bounded_json(self.api(&format!("{}/user/emails?per_page=100", self.api_url), token).await?).await?;
        Ok(super::Identity { subject: user.id.to_string(), signup_email: signup_email(&emails) })
    }

    async fn api(&self, url: &str, token: &str) -> Result<reqwest::Response, Rejected> {
        self.http
            .get(url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await
            .map_err(|_| Rejected)
    }

    /// OAuth App tokens do not expire; we never store this one, so revoke it.
    /// Best effort: a failure does not affect sign-in.
    async fn revoke(&self, token: &str) {
        let _ = self
            .http
            .delete(format!("{}/applications/{}/token", self.api_url, self.config.client_id))
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .json(&serde_json::json!({ "access_token": token }))
            .send()
            .await;
    }
}

impl super::Client for Client {
    fn authorization_url(&self, state: &str, _nonce: &str, verifier: &str) -> String {
        let mut url = reqwest::Url::parse("https://github.com/login/oauth/authorize").expect("constant URL");
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("scope", "user:email"),
            ("state", state),
            ("code_challenge", &b64url_encode(&Sha256::digest(verifier.as_bytes()))),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ]);
        url.into()
    }

    fn identify<'a>(&'a self, code: &'a str, verifier: &'a str, _nonce: &'a str) -> super::Exchange<'a> {
        Box::pin(self.exchange(code, verifier))
    }
}

#[cfg(test)]
pub(super) mod tests;
