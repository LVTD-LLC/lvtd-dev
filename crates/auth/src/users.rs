//! The `users` table and the two things you can do with it: sign up and
//! check credentials.

use crate::password;
use app_core::Pool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub created_at: OffsetDateTime,
    /// Loaded from the database; never accepted from public auth forms.
    pub is_admin: bool,
    /// Storage key of the profile picture, if one was uploaded (`app_core::storage`).
    pub avatar_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SignUpError {
    #[error("Enter a valid email address.")]
    InvalidEmail,
    #[error("Password must be at least {} characters.", password::MIN_LEN)]
    PasswordTooShort,
    #[error("Password is too long.")]
    PasswordTooLong,
    #[error("An account with that email already exists.")]
    EmailTaken,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Lower-case + trim; the DB CHECK constraint enforces the same shape.
pub fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') || email.len() > 254 {
        return None;
    }
    Some(email)
}

pub async fn sign_up(pool: &Pool, email: &str, password: String) -> Result<User, SignUpError> {
    let email = normalize_email(email).ok_or(SignUpError::InvalidEmail)?;
    if password.chars().count() < password::MIN_LEN {
        return Err(SignUpError::PasswordTooShort);
    }
    if password.len() > password::MAX_LEN {
        return Err(SignUpError::PasswordTooLong);
    }
    let hash = password::hash(password).await?;
    let id = Uuid::now_v7();
    let row = sqlx::query_as!(
        User,
        r#"INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)
           ON CONFLICT (email) DO NOTHING
           RETURNING id, email, created_at, is_admin, avatar_key"#,
        id,
        email,
        hash
    )
    .fetch_optional(pool)
    .await
    .map_err(anyhow::Error::from)?;
    row.ok_or(SignUpError::EmailTaken)
}

/// The user with this id, if it still exists.
pub async fn find(pool: &Pool, id: Uuid) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as!(User, "SELECT id, email, created_at, is_admin, avatar_key FROM users WHERE id = $1", id)
        .fetch_optional(pool)
        .await?)
}

/// Returns the user when the email exists and the password matches. Takes
/// the same time either way, so it does not reveal which emails exist.
pub async fn verify_credentials(pool: &Pool, email: &str, password: String) -> anyhow::Result<Option<User>> {
    let email = normalize_email(email).unwrap_or_default();
    let row = sqlx::query!(
        "SELECT id, email, created_at, password_hash, is_admin, avatar_key FROM users WHERE email = $1",
        email
    )
    .fetch_optional(pool)
    .await?;
    let hash = row.as_ref().map(|r| r.password_hash.clone());
    let ok = password::verify(password, hash).await?;
    Ok(match row {
        Some(r) if ok => Some(User {
            id: r.id,
            email: r.email,
            created_at: r.created_at,
            is_admin: r.is_admin,
            avatar_key: r.avatar_key,
        }),
        _ => None,
    })
}
