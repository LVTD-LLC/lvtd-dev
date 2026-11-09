//! Google as an [`social_login::Client`](super::Client): OpenID Connect with RS256 ID
//! tokens verified against cached Google keys. Secrets and provider responses
//! never enter error messages.
use super::{Rejected, bounded_json};
use app_auth::webauthn::b64url_encode;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
}
impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::parse(
            std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
            std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
        )
    }
    fn parse(client_id: String, client_secret: String) -> anyhow::Result<Option<Self>> {
        if client_id.is_empty() && client_secret.is_empty() {
            return Ok(None);
        }
        anyhow::ensure!(
            !client_id.trim().is_empty() && !client_secret.trim().is_empty(),
            "Set GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET together"
        );
        anyhow::ensure!(
            client_id.ends_with(".apps.googleusercontent.com") && !client_id.chars().any(char::is_whitespace),
            "Invalid GOOGLE_CLIENT_ID"
        );
        Ok(Some(Self { client_id, client_secret }))
    }
}

pub struct Client {
    config: Config,
    http: reqwest::Client,
    redirect_uri: String,
    token_url: String,
    keys_url: String,
    keys: Mutex<Keys>,
}
struct Keys {
    set: JwkSet,
    expires: Instant,
    last_attempt: Option<Instant>,
}

/// Verified ID token claims.
#[derive(Clone, Deserialize)]
pub(super) struct Claims {
    pub sub: String,
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
    pub hd: Option<String>,
    pub nonce: String,
    pub azp: Option<String>,
    pub iat: i64,
}
impl Claims {
    /// Google is not authoritative for third-party email domains, even when
    /// email_verified is true. Such users can link after local email signup.
    pub fn signup_email(&self) -> Option<String> {
        authoritative_email(self.email.as_deref()?, self.email_verified, self.hd.as_deref())
    }
}

/// Apply Google's email-authority rules to claims from an already verified ID
/// token. This does not authenticate a token or authorize account linking.
pub fn authoritative_email(raw: &str, verified: bool, hosted_domain: Option<&str>) -> Option<String> {
    let email = app_auth::users::normalize_email(raw)?;
    (verified
        && (email.ends_with("@gmail.com") || hosted_domain.is_some_and(|s| !s.is_empty()))
        && email.matches('@').count() == 1
        && !email.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then_some(email)
}

impl Client {
    pub fn new(config: Config, site_url: &str) -> anyhow::Result<Self> {
        let now = Instant::now();
        Ok(Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            redirect_uri: super::redirect_uri(site_url, "google")?,
            token_url: "https://oauth2.googleapis.com/token".into(),
            keys_url: "https://www.googleapis.com/oauth2/v3/certs".into(),
            keys: Mutex::new(Keys { set: JwkSet { keys: vec![] }, expires: now, last_attempt: None }),
        })
    }

    async fn exchange(&self, code: &str, verifier: &str, nonce: &str) -> Result<Claims, Rejected> {
        #[derive(Deserialize)]
        struct Token {
            id_token: String,
        }
        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", &self.config.client_id),
                ("client_secret", &self.config.client_secret),
                ("redirect_uri", &self.redirect_uri),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| Rejected)?;
        let token: Token = bounded_json(response).await?;
        self.verify(&token.id_token, nonce).await
    }

    async fn verify(&self, token: &str, nonce: &str) -> Result<Claims, Rejected> {
        if token.len() > 16_384 {
            return Err(Rejected);
        }
        let header = decode_header(token).map_err(|_| Rejected)?;
        if header.alg != Algorithm::RS256 {
            return Err(Rejected);
        }
        let kid = header.kid.ok_or(Rejected)?;
        let key = {
            let mut keys = self.keys.lock().await;
            let now = Instant::now();
            if now >= keys.expires || keys.set.find(&kid).is_none() {
                // Bound unknown-key refreshes, including failures; prevent refresh stampedes.
                if keys.last_attempt.is_none_or(|last| now.duration_since(last) >= Duration::from_secs(30)) {
                    keys.last_attempt = Some(now);
                    let response = self.http.get(&self.keys_url).send().await.map_err(|_| Rejected)?;
                    let ttl = response
                        .headers()
                        .get(reqwest::header::CACHE_CONTROL)
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.split(',').find_map(|v| v.trim().strip_prefix("max-age=")?.parse::<u64>().ok()))
                        .unwrap_or(300)
                        .min(86_400);
                    let set: JwkSet = bounded_json(response).await?;
                    if set.keys.is_empty() || set.keys.len() > 32 {
                        return Err(Rejected);
                    }
                    keys.set = set;
                    keys.expires = Instant::now() + Duration::from_secs(ttl);
                }
                if Instant::now() >= keys.expires {
                    return Err(Rejected);
                }
            }
            DecodingKey::from_jwk(keys.set.find(&kid).ok_or(Rejected)?).map_err(|_| Rejected)?
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.config.client_id]);
        validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub", "iat"]);
        validation.leeway = 30;
        let identity = decode::<Claims>(token, &key, &validation).map_err(|_| Rejected)?.claims;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if identity.nonce != nonce
            || identity.sub.is_empty()
            || identity.sub.len() > 255
            || identity.azp.as_ref().is_some_and(|azp| azp != &self.config.client_id)
            || identity.iat > now + 30
            || identity.iat < now - 600
        {
            return Err(Rejected);
        }
        Ok(identity)
    }
}

impl super::Client for Client {
    fn authorization_url(&self, state: &str, nonce: &str, verifier: &str) -> String {
        let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("constant URL");
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", "openid email"),
            ("state", state),
            ("nonce", nonce),
            ("code_challenge", &b64url_encode(&Sha256::digest(verifier.as_bytes()))),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ]);
        url.into()
    }

    fn identify<'a>(&'a self, code: &'a str, verifier: &'a str, nonce: &'a str) -> super::Exchange<'a> {
        Box::pin(async move {
            let claims = self.exchange(code, verifier, nonce).await?;
            Ok(super::Identity { signup_email: claims.signup_email(), subject: claims.sub })
        })
    }
}

#[cfg(test)]
pub(super) mod tests;
