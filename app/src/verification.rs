//! Pending signup and email delivery: Mailgun when configured, otherwise the
//! console (the log) for local development. Never create an auth session here.
use crate::views::SITE_NAME;
use app_auth::{User, password, users};
use app_core::Pool;
use axum::http::{HeaderMap, HeaderValue, header};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const COOKIE: &str = "signup";
const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Keys codes when the console transport runs without `EMAIL_VERIFICATION_KEY`.
/// It never protects real traffic: the console transport is refused on HTTPS origins.
const DEVELOPMENT_HASH_KEY: &str = "development-only-email-verification-key-never-production";

pub struct Config {
    pub api_key: String,
    pub domain: String,
    pub from: String,
    pub hash_key: String,
}
impl Config {
    /// Mailgun configuration, or `None` when the three Mailgun variables are blank.
    /// A partial configuration is an error so a typo never silently falls back.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let get = |name| std::env::var(name).unwrap_or_default();
        let values = [get("MAILGUN_SENDING_KEY"), get("MAILGUN_DOMAIN"), get("MAIL_FROM")];
        if values.iter().all(String::is_empty) {
            return Ok(None);
        }
        let hash_key = get("EMAIL_VERIFICATION_KEY");
        anyhow::ensure!(
            values.iter().all(|v| !v.is_empty()) && !hash_key.is_empty(),
            "Incomplete email configuration: set MAILGUN_SENDING_KEY, MAILGUN_DOMAIN, MAIL_FROM and EMAIL_VERIFICATION_KEY together"
        );
        check_hash_key(&hash_key)?;
        anyhow::ensure!(
            values[1].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
            "Invalid Mailgun domain"
        );
        let [api_key, domain, from] = values;
        Ok(Some(Self { api_key, domain, from, hash_key }))
    }
}
fn check_hash_key(hash_key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(hash_key.len() >= 32, "EMAIL_VERIFICATION_KEY must contain at least 32 random characters");
    Ok(())
}
enum Transport {
    Mailgun {
        http: reqwest::Client,
        endpoint: String,
        api_key: String,
        from: String,
    },
    /// Writes each message to the log instead of sending it. Local development only.
    Console,
}
pub struct Client {
    hash_key: String,
    transport: Transport,
}
impl Client {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let Config { api_key, domain, from, hash_key } = config;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let endpoint = format!("https://api.mailgun.net/v3/{domain}/messages");
        Ok(Self { hash_key, transport: Transport::Mailgun { http, endpoint, api_key, from } })
    }
    /// Dependency injection for local HTTP delivery tests; production always uses Mailgun HTTPS.
    pub fn with_endpoint(config: Config, endpoint: String) -> anyhow::Result<Self> {
        let mut client = Self::new(config)?;
        if let Transport::Mailgun { endpoint: target, .. } = &mut client.transport {
            *target = endpoint;
        }
        Ok(client)
    }
    /// The console transport: every message, verification codes included, goes
    /// to the log at WARN. Nothing is sent. Only for local development.
    pub fn console(hash_key: String) -> Self {
        Self { hash_key, transport: Transport::Console }
    }
    /// Mailgun when configured. Otherwise the console transport, but only on a
    /// plain-HTTP `SITE_URL`: a deployment that forgot its mail credentials must
    /// fail to start rather than write verification codes into production logs.
    pub fn from_env(site_url: &str) -> anyhow::Result<Self> {
        match Config::from_env()? {
            Some(config) => Self::new(config),
            None => Self::fallback(std::env::var("EMAIL_VERIFICATION_KEY").unwrap_or_default(), site_url),
        }
    }
    fn fallback(hash_key: String, site_url: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !site_url.get(..8).is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://")),
            "Email delivery is not configured. Set MAILGUN_SENDING_KEY, MAILGUN_DOMAIN, MAIL_FROM and EMAIL_VERIFICATION_KEY; the console transport is only allowed on a non-HTTPS SITE_URL"
        );
        let hash_key = if hash_key.is_empty() {
            DEVELOPMENT_HASH_KEY.to_string()
        } else {
            check_hash_key(&hash_key)?;
            hash_key
        };
        tracing::warn!(
            "No email provider configured: emails and verification codes will be printed to this log. Configure Mailgun before deploying."
        );
        Ok(Self::console(hash_key))
    }
    fn digest(&self, context: &str, value: &str) -> Vec<u8> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.hash_key.as_bytes()).expect("HMAC key");
        mac.update(context.as_bytes());
        mac.update(&[0]);
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
    async fn send(&self, email: &str, code: &str) -> bool {
        let text = format!(
            "Your {SITE_NAME} verification code is: {code}\n\nEnter it in the browser where you signed up. This code expires in 10 minutes and can only be used once. Never share it.\n\nIf you did not request this, ignore this email. No account has been created."
        );
        let subject = format!("Verify your {SITE_NAME} email");
        match self.send_message(email, &subject, &text).await {
            Ok(()) => true,
            Err(_) => {
                tracing::warn!("Verification email delivery was not accepted");
                false
            }
        }
    }
    /// Deliver a plain-text email through the configured provider. Tracking is
    /// off; with Mailgun nothing about the message or recipient is logged, and
    /// errors carry the HTTP status only, so background jobs can retry them
    /// safely. The console transport logs the whole message on purpose.
    pub async fn send_message(&self, to: &str, subject: &str, text: &str) -> anyhow::Result<()> {
        let Transport::Mailgun { http, endpoint, api_key, from } = &self.transport else {
            tracing::warn!("Console email (not sent)\nTo: {to}\nSubject: {subject}\n\n{text}");
            return Ok(());
        };
        // Do not log request/response bodies, recipient, codes or reqwest errors (URLs).
        let response = http
            .post(endpoint)
            .basic_auth("api", Some(api_key))
            .form(&[
                ("from", from.as_str()),
                ("to", to),
                ("subject", subject),
                ("text", text),
                ("o:tracking", "no"),
                ("o:tracking-clicks", "no"),
                ("o:tracking-opens", "no"),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("mail delivery transport failed"))?;
        anyhow::ensure!(response.status().is_success(), "mail delivery was not accepted (HTTP {})", response.status());
        Ok(())
    }
}
fn random(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut v = vec![0; n];
    getrandom::fill(&mut v)?;
    Ok(v)
}
fn code() -> anyhow::Result<String> {
    Ok(random(6)?.iter().map(|b| ALPHABET[(b & 31) as usize] as char).collect())
}
fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
pub fn token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|v| v.trim().strip_prefix("signup="))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}
pub fn cookie(token: &str, secure: bool) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
        if token.is_empty() { 0 } else { 86400 },
        secure = if secure { "; Secure" } else { "" }
    ))
    .expect("random hex cookie")
}
#[derive(Debug)]
pub enum Start {
    Pending { token: String, delivered: bool },
    Limited,
}

async fn budget(pool: &Pool, key: Vec<u8>, maximum: i32) -> anyhow::Result<bool> {
    let row = sqlx::query!(r#"INSERT INTO signup_limits (key) VALUES ($1)
        ON CONFLICT (key) DO UPDATE SET
        requests = CASE WHEN signup_limits.window_start <= now() - interval '1 hour' THEN 1 ELSE signup_limits.requests + 1 END,
        window_start = CASE WHEN signup_limits.window_start <= now() - interval '1 hour' THEN now() ELSE signup_limits.window_start END
        RETURNING requests"#, key).fetch_one(pool).await?;
    Ok(row.requests <= maximum)
}

pub async fn start(
    pool: &Pool,
    client: &Client,
    email: &str,
    raw_password: String,
) -> Result<Start, users::SignUpError> {
    let email = users::normalize_email(email)
        .filter(|email| {
            let Some((local, domain)) = email.split_once('@') else {
                return false;
            };
            !local.starts_with('.')
                && !local.ends_with('.')
                && !local.contains("..")
                && local.bytes().all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
        .ok_or(users::SignUpError::InvalidEmail)?;
    if raw_password.chars().count() < password::MIN_LEN {
        return Err(users::SignUpError::PasswordTooShort);
    }
    if raw_password.len() > password::MAX_LEN {
        return Err(users::SignUpError::PasswordTooLong);
    }
    async {
        if !budget(pool, client.digest("limit", "global"), 200).await?
            || !budget(pool, client.digest("email", &email), 5).await?
        {
            return Ok(Start::Limited);
        }
        let password_hash = password::hash(raw_password).await?;
        let token = hex::encode(random(32)?);
        let token_hash = token_hash(&token);
        let code = code()?;
        let code_hash = client.digest(&token, &code);
        // Do not expose existing accounts or replace their credentials. Same pending response.
        let exists = sqlx::query!("SELECT id FROM users WHERE email = $1", email).fetch_optional(pool).await?.is_some();
        if exists {
            return Ok(Start::Pending { token, delivered: true });
        }
        let row = sqlx::query!(
            r#"INSERT INTO pending_signups (email, password_hash, token_hash, code_hash, expires_at)
            VALUES ($1, $2, $3, $4, now() + interval '10 minutes')
            ON CONFLICT (email) DO UPDATE SET password_hash = EXCLUDED.password_hash,
              token_hash = EXCLUDED.token_hash, code_hash = EXCLUDED.code_hash,
              expires_at = EXCLUDED.expires_at, last_sent_at = now(), created_at = now(), attempts = 0, delivered = false
            WHERE pending_signups.last_sent_at <= now() - interval '1 minute'
            RETURNING email"#,
            email,
            password_hash,
            token_hash,
            code_hash
        )
        .fetch_optional(pool)
        .await?;
        if row.is_none() {
            return Ok(Start::Limited);
        }
        let delivered = deliver(pool, client, &email, &token_hash, &code_hash, &code).await?;
        Ok(Start::Pending { token, delivered })
    }
    .await
    .map_err(users::SignUpError::Other)
}
async fn deliver(
    pool: &Pool,
    client: &Client,
    email: &str,
    token_hash: &[u8],
    code_hash: &[u8],
    code: &str,
) -> anyhow::Result<bool> {
    let delivered = client.send(email, code).await;
    sqlx::query!(
        "UPDATE pending_signups SET delivered = $3 WHERE token_hash = $1 AND code_hash = $2",
        token_hash,
        code_hash,
        delivered
    )
    .execute(pool)
    .await?;
    Ok(delivered)
}
pub async fn resend(pool: &Pool, client: &Client, token: &str) -> anyhow::Result<Start> {
    let token_hash = token_hash(token);
    let row = sqlx::query!(
        "SELECT email FROM pending_signups WHERE token_hash = $1 AND created_at > now() - interval '1 day'",
        token_hash
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(Start::Limited);
    };
    if !budget(pool, client.digest("limit", "global"), 200).await?
        || !budget(pool, client.digest("email", &row.email), 5).await?
    {
        return Ok(Start::Limited);
    }
    let code = code()?;
    let code_hash = client.digest(token, &code);
    let changed = sqlx::query!(
        r#"UPDATE pending_signups SET code_hash = $2, expires_at = now() + interval '10 minutes',
        last_sent_at = now(), created_at = now(), attempts = 0, delivered = false
        WHERE token_hash = $1 AND last_sent_at <= now() - interval '1 minute' RETURNING email"#,
        token_hash,
        code_hash
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = changed else {
        return Ok(Start::Limited);
    };
    let delivered = deliver(pool, client, &row.email, &token_hash, &code_hash, &code).await?;
    Ok(Start::Pending { token: token.into(), delivered })
}
pub async fn verify(pool: &Pool, client: &Client, token: &str, code: &str) -> anyhow::Result<Option<User>> {
    let mut tx = pool.begin().await?;
    let token_hash = token_hash(token);
    // A row lock makes attempts and consumption atomic across all replicas and concurrent requests.
    let row = sqlx::query!(
        r#"SELECT email, password_hash, code_hash FROM pending_signups
        WHERE token_hash = $1 AND expires_at > now() AND attempts < 5 AND delivered FOR UPDATE"#,
        token_hash
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let normalized = code.trim().to_ascii_uppercase();
    let mut mac = Hmac::<Sha256>::new_from_slice(client.hash_key.as_bytes()).expect("HMAC key");
    mac.update(token.as_bytes());
    mac.update(&[0]);
    mac.update(normalized.as_bytes());
    if normalized.len() != 6 || mac.verify_slice(&row.code_hash).is_err() {
        sqlx::query!("UPDATE pending_signups SET attempts = attempts + 1 WHERE token_hash = $1", token_hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    let id = Uuid::now_v7();
    let user = sqlx::query_as!(
        User,
        r#"INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)
        ON CONFLICT (email) DO NOTHING RETURNING id, email, created_at, is_admin, avatar_key"#,
        id,
        row.email,
        row.password_hash
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM pending_signups WHERE token_hash = $1", token_hash).execute(&mut *tx).await?;
    if let Some(user) = &user {
        // Same transaction as the account: committed together or not at all.
        app_core::jobs::enqueue(
            &mut *tx,
            &crate::jobs::SendWelcomeEmail { user_id: user.id },
            app_core::jobs::Enqueue { dedupe_key: Some(user.id.to_string()), ..Default::default() },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(user)
}
pub async fn remaining(pool: &Pool, token: &str) -> anyhow::Result<i64> {
    let row = sqlx::query!("SELECT last_sent_at FROM pending_signups WHERE token_hash = $1", token_hash(token))
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (60 - (time::OffsetDateTime::now_utc() - r.last_sent_at).whole_seconds()).clamp(0, 60)).unwrap_or(0))
}
pub async fn cleanup(pool: Pool) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        interval.tick().await;
        let pending = sqlx::query!("DELETE FROM pending_signups WHERE created_at <= now() - interval '1 day'")
            .execute(&pool)
            .await;
        let limits = sqlx::query!("DELETE FROM signup_limits WHERE window_start <= now() - interval '1 day'")
            .execute(&pool)
            .await;
        if pending.is_err() || limits.is_err() {
            tracing::warn!("Signup cleanup failed; retrying next hour");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_transport_is_the_development_default() {
        let client = Client::fallback(String::new(), "http://localhost:3000").unwrap();
        assert!(matches!(client.transport, Transport::Console));
        assert_eq!(client.hash_key, DEVELOPMENT_HASH_KEY);
    }

    #[test]
    fn console_transport_keeps_a_configured_hash_key() {
        let key = "a".repeat(32);
        let client = Client::fallback(key.clone(), "http://localhost:3000").unwrap();
        assert_eq!(client.hash_key, key);
        assert!(Client::fallback("short".into(), "http://localhost:3000").is_err());
    }

    #[test]
    fn console_transport_is_refused_on_https_origins() {
        let Err(error) = Client::fallback(String::new(), "https://example.com") else {
            panic!("console transport on https")
        };
        assert!(error.to_string().contains("MAILGUN_SENDING_KEY"), "{error}");
        assert!(Client::fallback("a".repeat(32), "https://example.com").is_err());
        assert!(Client::fallback(String::new(), "HTTPS://example.com").is_err());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn global_send_limit_preserves_pending_state_and_recovers_next_hour(pool: Pool) {
        let client = Client::console("test-global-budget-key-long-enough".into());
        let Start::Pending { token, delivered: true } =
            start(&pool, &client, "pending@example.com", "correct-horse".into()).await.unwrap()
        else {
            panic!("pending signup")
        };
        let global = client.digest("limit", "global");
        for _ in 1..200 {
            assert!(budget(&pool, global.clone(), 200).await.unwrap());
        }
        let before = sqlx::query!("SELECT token_hash, code_hash, last_sent_at FROM pending_signups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(matches!(
            start(&pool, &client, "new@example.com", "correct-horse".into()).await.unwrap(),
            Start::Limited
        ));
        assert!(matches!(resend(&pool, &client, &token).await.unwrap(), Start::Limited));
        let after = sqlx::query!("SELECT token_hash, code_hash, last_sent_at FROM pending_signups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (before.token_hash, before.code_hash, before.last_sent_at),
            (after.token_hash, after.code_hash, after.last_sent_at)
        );
        assert_eq!(
            sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
            Some(1)
        );
        assert_eq!(sqlx::query!("SELECT count(*) AS count FROM users").fetch_one(&pool).await.unwrap().count, Some(0));
        // A new window restores capacity without clearing pending verification data.
        sqlx::query!("UPDATE signup_limits SET window_start = now() - interval '61 minutes' WHERE key = $1", global)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            start(&pool, &client, "new@example.com", "correct-horse".into()).await.unwrap(),
            Start::Pending { delivered: true, .. }
        ));
        assert_eq!(
            sqlx::query!("SELECT count(*) AS count FROM pending_signups").fetch_one(&pool).await.unwrap().count,
            Some(2)
        );
    }
}
