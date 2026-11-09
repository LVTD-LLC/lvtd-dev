//! Personal API keys: the `api_keys` table and bearer-token lookup.
//!
//! A key is `sr_` followed by 64 hex characters (256 random bits). The user
//! sees the full key once, when it is created; the table keeps only its
//! SHA-256 and a short prefix for display. Revoking a key deletes its row,
//! so revocation takes effect on the next request.

use app_auth::User;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const PREFIX: &str = "sr_";
pub const NAME_MAX: usize = 60;
pub const DEFAULT_NAME: &str = "API key";
/// Keys per account. Enough for a few machines and agents; prevents unbounded growth.
pub const MAX_PER_USER: i64 = 10;
/// Characters of the token shown in the key list (`sr_` plus eight hex digits).
const DISPLAY_PREFIX_LEN: usize = PREFIX.len() + 8;

#[derive(Clone, Debug)]
pub struct ApiKey {
    pub id: Uuid,
    pub prefix: String,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Use a key name of {NAME_MAX} characters or fewer, without control characters.")]
    InvalidName,
    #[error("You already have {MAX_PER_USER} API keys. Revoke one before creating another.")]
    TooMany,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for CreateError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn random_token() -> anyhow::Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw)?;
    Ok(format!("{PREFIX}{}", hex::encode(raw)))
}

/// Create a key and return the plaintext token exactly once. A blank name
/// becomes [`DEFAULT_NAME`].
#[tracing::instrument(name = "api_key.create", skip_all)]
pub async fn create(pool: &PgPool, user_id: Uuid, name: &str) -> Result<(ApiKey, String), CreateError> {
    let name = name.trim();
    if name.chars().count() > NAME_MAX || name.chars().any(char::is_control) {
        return Err(CreateError::InvalidName);
    }
    let name = if name.is_empty() { DEFAULT_NAME } else { name };
    let token = random_token()?;
    let prefix = &token[..DISPLAY_PREFIX_LEN];
    let mut tx = pool.begin().await?;
    // Serialize concurrent creations per user so the limit cannot be exceeded.
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user_id).fetch_one(&mut *tx).await?;
    let count = sqlx::query!(r#"SELECT count(*) AS "count!" FROM api_keys WHERE user_id = $1"#, user_id)
        .fetch_one(&mut *tx)
        .await?
        .count;
    if count >= MAX_PER_USER {
        return Err(CreateError::TooMany);
    }
    let key = sqlx::query_as!(
        ApiKey,
        r#"INSERT INTO api_keys (id, user_id, token_hash, prefix, name) VALUES ($1, $2, $3, $4, $5)
           RETURNING id, prefix, name, created_at, last_used_at"#,
        Uuid::now_v7(),
        user_id,
        token_hash(&token),
        prefix,
        name
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((key, token))
}

#[tracing::instrument(name = "api_key.list_for_user", skip_all)]
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> Result<Vec<ApiKey>, sqlx::Error> {
    sqlx::query_as!(
        ApiKey,
        "SELECT id, prefix, name, created_at, last_used_at FROM api_keys WHERE user_id = $1 ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(pool)
    .await
}

/// Returns whether a row was deleted. Someone else's key is simply not found.
#[tracing::instrument(name = "api_key.revoke", skip_all)]
pub async fn revoke(pool: &PgPool, id: Uuid, user_id: Uuid) -> Result<bool, sqlx::Error> {
    let n = sqlx::query!("DELETE FROM api_keys WHERE id = $1 AND user_id = $2", id, user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// Resolve a bearer token to its owner, or `None` if unknown. Records usage
/// at most once a minute so authenticating stays a single statement without
/// a write on every request.
#[tracing::instrument(name = "api_key.authenticate", skip_all)]
pub async fn authenticate(pool: &PgPool, token: &str) -> Result<Option<User>, sqlx::Error> {
    if !token.starts_with(PREFIX) || token.len() > 128 {
        return Ok(None);
    }
    let hash = token_hash(token);
    let row = sqlx::query!(
        r#"WITH touched AS (
               UPDATE api_keys SET last_used_at = now()
               WHERE token_hash = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')
           )
           SELECT u.id, u.email, u.created_at, u.is_admin, u.avatar_key
           FROM api_keys k JOIN users u ON u.id = k.user_id
           WHERE k.token_hash = $1"#,
        hash
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| User {
        id: r.id,
        email: r.email,
        created_at: r.created_at,
        is_admin: r.is_admin,
        avatar_key: r.avatar_key,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_prefixed_random_and_sized() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), PREFIX.len() + 64);
        assert!(a.starts_with(PREFIX));
        assert!(a[PREFIX.len()..].bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(a[..DISPLAY_PREFIX_LEN].len(), 11);
    }
}
