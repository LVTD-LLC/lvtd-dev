//! Postgres connection pool. The app owns its migrations: it embeds its
//! `migrations/` directory with `sqlx::migrate!` and passes the migrator
//! here, so this crate works for any app built on it without knowing where
//! that app's schema lives.

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

pub type Pool = sqlx::PgPool;

/// Connect and apply the app's pending migrations.
pub async fn connect(database_url: &str, migrator: &Migrator) -> anyhow::Result<Pool> {
    let pool =
        PgPoolOptions::new().max_connections(16).acquire_timeout(Duration::from_secs(5)).connect(database_url).await?;
    migrator.run(&pool).await?;
    Ok(pool)
}
