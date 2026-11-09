//! Admin authorization and a read-only account overview.
use crate::{
    AppError, SharedState,
    routes::html,
    views::{self, Page},
};
use app_auth::{CurrentUser, User, users};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const PAGE_SIZE: usize = 50;

pub struct Account {
    pub id: Uuid,
    pub email: String,
    pub created_at: OffsetDateTime,
    pub is_admin: bool,
}

/// Check the role loaded by CurrentUser for this request before any admin operation.
pub fn require_admin(user: &User) -> Result<(), AppError> {
    if !user.is_admin {
        return Err(AppError::Status(StatusCode::FORBIDDEN, "Admin access required.".into()));
    }
    Ok(())
}

/// Operator CLI only: changes an existing account, never creates one.
pub async fn set_admin(pool: &PgPool, email: &str, enabled: bool) -> anyhow::Result<()> {
    let email = users::normalize_email(email).ok_or_else(|| anyhow::anyhow!("Enter a valid email address."))?;
    let result = sqlx::query!("UPDATE users SET is_admin = $2, updated_at = now() WHERE email = $1", email, enabled)
        .execute(pool)
        .await?;
    anyhow::ensure!(result.rows_affected() == 1, "No account with that email exists.");
    Ok(())
}

#[derive(Default, Deserialize)]
pub struct Pagination {
    before: Option<Uuid>,
}

pub async fn page(
    State(state): State<SharedState>,
    CurrentUser(user): CurrentUser,
    Query(pagination): Query<Pagination>,
) -> Result<Response, AppError> {
    require_admin(&user)?;
    let mut accounts = sqlx::query_as!(
        Account,
        r#"SELECT id, email, created_at, is_admin
           FROM users WHERE id < $1 ORDER BY id DESC LIMIT $2"#,
        pagination.before.unwrap_or(Uuid::max()),
        (PAGE_SIZE + 1) as i64
    )
    .fetch_all(&state.pool)
    .await?;
    let has_more = accounts.len() > PAGE_SIZE;
    accounts.truncate(PAGE_SIZE);
    let next = has_more.then(|| accounts.last().expect("full page").id);
    let body = views::admin::page(&accounts, next, pagination.before.is_some());
    let mut response =
        html(StatusCode::OK, views::layout(&state, Page { title: "Admin", path: "/admin", user: Some(&user), body }));
    response.headers_mut().insert("x-robots-tag", "noindex, nofollow".parse().unwrap());
    Ok(response)
}
