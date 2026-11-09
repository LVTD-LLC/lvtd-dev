//! Cookie sessions backed by the `sessions` table.
//!
//! The cookie carries a 256-bit random token; the table stores its SHA-256.
//! Looking a session up is one indexed query joined to `users`.

use crate::users::User;
use app_core::Pool;
use axum::http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub const COOKIE_NAME: &str = "session";

#[derive(Clone, Debug)]
pub struct Session {
    pub id: Uuid,
    pub user: User,
    pub expires_at: OffsetDateTime,
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Create a session and return the raw token to put in the cookie.
pub async fn create(pool: &Pool, user_id: Uuid, ttl: Duration) -> anyhow::Result<String> {
    let token = crate::random_hex(32)?;
    let expires_at = OffsetDateTime::now_utc() + ttl;
    sqlx::query!(
        "INSERT INTO sessions (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
        Uuid::now_v7(),
        user_id,
        token_hash(&token),
        expires_at
    )
    .execute(pool)
    .await?;
    Ok(token)
}

/// Resolve a raw token to a live session, or `None` if unknown or expired.
pub async fn lookup(pool: &Pool, token: &str) -> anyhow::Result<Option<Session>> {
    let row = sqlx::query!(
        r#"SELECT s.id AS session_id, s.expires_at, u.id AS user_id, u.email, u.created_at, u.is_admin, u.avatar_key
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.token_hash = $1 AND s.expires_at > now()"#,
        token_hash(token)
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Session {
        id: r.session_id,
        expires_at: r.expires_at,
        user: User {
            id: r.user_id,
            email: r.email,
            created_at: r.created_at,
            is_admin: r.is_admin,
            avatar_key: r.avatar_key,
        },
    }))
}

pub async fn delete(pool: &Pool, token: &str) -> anyhow::Result<()> {
    sqlx::query!("DELETE FROM sessions WHERE token_hash = $1", token_hash(token)).execute(pool).await?;
    Ok(())
}

/// Remove expired rows.
pub async fn purge_expired(pool: &Pool) -> anyhow::Result<u64> {
    Ok(sqlx::query!("DELETE FROM sessions WHERE expires_at <= now()").execute(pool).await?.rows_affected())
}

/// Purge immediately, then hourly, without blocking the HTTP listener's startup.
/// Spawn once per server and abort the task when the server stops. Failures are
/// logged and retried on the next tick; missed ticks do not cause catch-up bursts.
pub async fn cleanup_expired(pool: Pool) {
    cleanup_expired_every(pool, std::time::Duration::from_secs(60 * 60)).await;
}

async fn cleanup_expired_every(pool: Pool, period: std::time::Duration) {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match purge_expired(&pool).await {
            Ok(purged) if purged > 0 => tracing::info!(purged, "removed expired sessions"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to remove expired sessions; will retry next tick"),
        }
    }
}

// ---------------------------------------------------------------------------
// Cookie plumbing
// ---------------------------------------------------------------------------

pub fn token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|line| {
        line.split(';').map(str::trim).find_map(|kv| kv.strip_prefix(COOKIE_NAME).and_then(|r| r.strip_prefix('=')))
    })
}

pub fn session_cookie(token: &str, ttl: Duration, secure: bool) -> HeaderValue {
    let mut v = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", ttl.whole_seconds());
    if secure {
        v.push_str("; Secure");
    }
    HeaderValue::from_str(&v).expect("hex token is a valid header value")
}

pub fn clear_cookie(secure: bool) -> HeaderValue {
    let mut v = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        v.push_str("; Secure");
    }
    HeaderValue::from_str(&v).expect("constant cookie string")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_removes_expired_sessions_on_successive_ticks(pool: Pool) {
        let user = crate::users::sign_up(&pool, "cleanup@example.com", "correct-horse".into()).await.unwrap();
        let live = create(&pool, user.id, Duration::days(1)).await.unwrap();
        create(&pool, user.id, Duration::seconds(-1)).await.unwrap();
        let task = tokio::spawn(cleanup_expired_every(pool.clone(), StdDuration::from_millis(20)));

        for pass in 0..2 {
            if pass == 1 {
                // A session added after the first cleanup must also be purged.
                create(&pool, user.id, Duration::seconds(-1)).await.unwrap();
            }
            tokio::time::timeout(StdDuration::from_secs(5), async {
                loop {
                    let count = sqlx::query!("SELECT count(*) AS count FROM sessions")
                        .fetch_one(&pool)
                        .await
                        .unwrap()
                        .count
                        .unwrap();
                    if count == 1 {
                        break;
                    }
                    tokio::time::sleep(StdDuration::from_millis(5)).await;
                }
            })
            .await
            .expect("periodic cleanup should remove expired rows");
            assert!(lookup(&pool, &live).await.unwrap().is_some());
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_keeps_running_after_database_errors(pool: Pool) {
        pool.close().await;
        let task = tokio::spawn(cleanup_expired_every(pool, StdDuration::from_millis(5)));
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        assert!(!task.is_finished(), "a failed purge must not terminate the worker");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}
