//! Every statement against the `jobs` table. Ownership is checked on each
//! update (`locked_by = $worker`), so a worker whose lease was taken over
//! cannot overwrite the new owner's bookkeeping.

use super::Enqueue;
use crate::Pool;
use time::OffsetDateTime;
use uuid::Uuid;

pub(crate) async fn insert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: Uuid,
    kind: &str,
    payload: &str,
    options: Enqueue,
    max_attempts: i32,
) -> sqlx::Result<Option<Uuid>> {
    let row = sqlx::query!(
        r#"INSERT INTO jobs (id, kind, payload, dedupe_key, run_at, max_attempts)
           VALUES ($1, $2, $3::text::jsonb, $4, COALESCE($5, now()), $6)
           ON CONFLICT (kind, dedupe_key) WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running') DO NOTHING
           RETURNING id"#,
        id,
        kind,
        payload,
        options.dedupe_key,
        options.run_at,
        max_attempts
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| r.id))
}

pub(crate) struct Claimed {
    pub id: Uuid,
    pub kind: String,
    pub payload: String,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// One statement, one round trip: the row lock lasts only as long as the
/// statement, and `SKIP LOCKED` keeps concurrent workers from waiting on
/// each other. Only kinds this worker can run are considered, so a binary
/// that predates a job kind never claims it.
pub(crate) async fn claim(pool: &Pool, worker: &str, lease_secs: f64, kinds: &[&str]) -> sqlx::Result<Option<Claimed>> {
    sqlx::query_as!(
        Claimed,
        r#"UPDATE jobs SET state = 'running', locked_by = $1, locked_until = now() + make_interval(secs => $2),
               attempts = attempts + 1
           WHERE id = (
               SELECT id FROM jobs
               WHERE state = 'queued' AND run_at <= now() AND kind = ANY($3)
               ORDER BY run_at
               LIMIT 1
               FOR UPDATE SKIP LOCKED
           )
           RETURNING id, kind, payload::text AS "payload!", attempts, max_attempts"#,
        worker,
        lease_secs,
        kinds as &[&str]
    )
    .fetch_optional(pool)
    .await
}

/// Extend the lease of a running job. `false` means this worker no longer owns it.
pub(crate) async fn heartbeat(pool: &Pool, id: Uuid, worker: &str, lease_secs: f64) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET locked_until = now() + make_interval(secs => $3)
         WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        id,
        worker,
        lease_secs
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub(crate) async fn complete(pool: &Pool, id: Uuid, worker: &str) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'done', finished_at = now(), locked_by = NULL, locked_until = NULL, last_error = NULL
         WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        id,
        worker
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub(crate) async fn retry_later(
    pool: &Pool,
    id: Uuid,
    worker: &str,
    delay_secs: f64,
    error: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'queued', run_at = now() + make_interval(secs => $3), last_error = $4,
             locked_by = NULL, locked_until = NULL
         WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        id,
        worker,
        delay_secs,
        error
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub(crate) async fn mark_dead(pool: &Pool, id: Uuid, worker: &str, error: &str) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'dead', finished_at = now(), last_error = $3, locked_by = NULL, locked_until = NULL
         WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        id,
        worker,
        error
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Jobs whose worker stopped heartbeating go back to the queue. The attempt
/// they were on already counts, so a job that keeps killing its worker
/// still ends up dead after `max_attempts`.
pub(crate) async fn requeue_expired(pool: &Pool) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             finished_at = CASE WHEN attempts >= max_attempts THEN now() ELSE NULL END,
             locked_by = NULL, locked_until = NULL, last_error = 'lease expired: worker stopped or crashed'
         WHERE state = 'running' AND locked_until < now()"
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Finished rows are kept for a while so `jobs stats` and operators can see them.
pub(crate) async fn purge_finished(pool: &Pool) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        "DELETE FROM jobs WHERE (state = 'done' AND finished_at < now() - interval '7 days')
            OR (state = 'dead' AND finished_at < now() - interval '30 days')"
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// One line per (state, kind) for the `jobs stats` command and admin pages.
#[derive(Debug)]
pub struct JobStats {
    pub state: String,
    pub kind: String,
    pub count: i64,
    /// Oldest `run_at` in this group: for `queued`, how far behind the queue is.
    pub oldest_run_at: Option<OffsetDateTime>,
}

pub async fn stats(pool: &Pool) -> sqlx::Result<Vec<JobStats>> {
    sqlx::query_as!(
        JobStats,
        r#"SELECT state, kind, count(*) AS "count!", min(run_at) AS oldest_run_at
           FROM jobs GROUP BY state, kind ORDER BY state, kind"#
    )
    .fetch_all(pool)
    .await
}

/// Give a dead job a fresh set of attempts. Returns `false` if the id is not a dead job.
pub async fn retry_dead(pool: &Pool, id: Uuid) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'queued', run_at = now(), attempts = 0, finished_at = NULL, last_error = NULL
         WHERE id = $1 AND state = 'dead'",
        id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
