//! The claim → run → record loop. One `Worker` per process; concurrency is a
//! semaphore inside it. A job runs outside any database transaction and
//! keeps its claim alive with heartbeats, so a crashed or killed worker
//! leaves nothing locked for longer than one lease.

use super::{Config, Context, JobError, Registry, WAKE, queue};
use crate::Pool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;
use tracing::Instrument;

/// Base and cap of the exponential backoff between attempts.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(600);
/// How often finished rows are purged.
const RETENTION_SWEEP: Duration = Duration::from_secs(3600);
/// After a local enqueue wakes the loop, give the enqueuing transaction a moment to commit.
const WAKE_GRACE: Duration = Duration::from_millis(200);
/// `last_error` is for operators, not for payload echoes or log dumps.
const MAX_ERROR_LEN: usize = 2048;

struct Inner<S> {
    pool: Pool,
    state: S,
    registry: Registry<S>,
    config: Config,
    id: String,
}

pub struct Worker<S> {
    inner: Arc<Inner<S>>,
}

impl<S: Clone + Send + Sync + 'static> Worker<S> {
    pub fn new(pool: Pool, state: S, registry: Registry<S>, config: Config) -> Self {
        let mut random = [0u8; 4];
        getrandom::fill(&mut random).expect("OS randomness");
        let host = std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "local".into());
        let id =
            format!("{host}-{}-{}", std::process::id(), random.iter().map(|b| format!("{b:02x}")).collect::<String>());
        Self { inner: Arc::new(Inner { pool, state, registry, config, id }) }
    }

    /// The worker's identity as stored in `jobs.locked_by`.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// Runs until `shutdown` becomes `true` (or its sender is dropped), then
    /// stops claiming and waits up to `Config::shutdown` for in-flight jobs.
    /// Jobs still running after that are left to lease expiry: another worker
    /// (or this one after a restart) runs them again.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let inner = self.inner;
        let kinds = inner.registry.kinds();
        let lease_secs = inner.config.lease.as_secs_f64();
        let semaphore = Arc::new(Semaphore::new(inner.config.concurrency.max(1)));
        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut last_lease_sweep: Option<Instant> = None;
        let mut last_retention_sweep: Option<Instant> = None;
        tracing::info!(worker = %inner.id, concurrency = inner.config.concurrency, ?kinds, "job worker started");

        while !*shutdown.borrow() {
            while let Some(finished) = tasks.try_join_next() {
                if let Err(error) = finished {
                    tracing::error!(%error, "job task ended abnormally");
                }
            }
            sweep(&inner, &mut last_lease_sweep, &mut last_retention_sweep).await;

            let permit = tokio::select! {
                permit = semaphore.clone().acquire_owned() => permit.expect("job semaphore is never closed"),
                _ = shutdown.changed() => break,
            };
            // Capacity and shutdown may become ready together. Recheck after
            // the wait so winning the permit branch cannot start another claim.
            if *shutdown.borrow() {
                break;
            }
            if kinds.is_empty() {
                drop(permit);
                wait(&inner.config, &mut shutdown).await;
                continue;
            }
            match queue::claim(&inner.pool, &inner.id, lease_secs, &kinds).await {
                Ok(Some(job)) => {
                    tasks.spawn(execute(inner.clone(), job, permit));
                    continue;
                }
                Ok(None) => drop(permit),
                Err(error) => {
                    drop(permit);
                    tracing::warn!(%error, "claiming a job failed; retrying after the poll interval");
                }
            }
            wait(&inner.config, &mut shutdown).await;
        }

        let in_flight = tasks.len();
        if in_flight > 0 {
            tracing::info!(in_flight, "job worker stopping; draining running jobs");
        }
        let drained = tokio::time::timeout(inner.config.shutdown, async {
            while let Some(finished) = tasks.join_next().await {
                if let Err(error) = finished {
                    tracing::error!(%error, "job task ended abnormally");
                }
            }
        })
        .await;
        if drained.is_err() {
            tracing::warn!(
                remaining = tasks.len(),
                "jobs still running at shutdown; their leases expire and other workers rerun them"
            );
        }
        tracing::info!(worker = %inner.id, "job worker stopped");
    }
}

/// Sleep until a local enqueue, the poll interval or shutdown, whichever comes first.
async fn wait(config: &Config, shutdown: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = WAKE.notified() => tokio::time::sleep(WAKE_GRACE).await,
        _ = tokio::time::sleep(config.poll) => {}
        _ = shutdown.changed() => {}
    }
}

async fn sweep<S>(inner: &Inner<S>, last_lease: &mut Option<Instant>, last_retention: &mut Option<Instant>) {
    let now = Instant::now();
    if last_lease.is_none_or(|t| now.duration_since(t) >= inner.config.poll) {
        *last_lease = Some(now);
        match queue::requeue_expired(&inner.pool).await {
            Ok(0) => {}
            Ok(count) => tracing::warn!(count, "requeued jobs whose lease expired"),
            Err(error) => tracing::warn!(%error, "lease sweep failed; retrying later"),
        }
    }
    if last_retention.is_none_or(|t| now.duration_since(t) >= RETENTION_SWEEP) {
        *last_retention = Some(now);
        match queue::purge_finished(&inner.pool).await {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "purged finished jobs past retention"),
            Err(error) => tracing::warn!(%error, "retention sweep failed; retrying later"),
        }
    }
}

async fn execute<S: Clone + Send + Sync + 'static>(
    inner: Arc<Inner<S>>,
    job: queue::Claimed,
    permit: OwnedSemaphorePermit,
) {
    let span = tracing::info_span!("job.run", kind = %job.kind, id = %job.id, attempt = job.attempts);
    async move {
        let _permit = permit;
        let Some(handler) = inner.registry.get(&job.kind) else {
            // Cannot happen: claims are filtered by registered kinds.
            record(&inner, &job, Err(JobError::Discard(anyhow::anyhow!("no handler for kind {}", job.kind)))).await;
            return;
        };
        // Stops with this future, including when the drain budget runs out and
        // the task is dropped mid-run: an orphaned heartbeat would keep the
        // lease alive forever and the job would never be rerun.
        let heartbeat = AbortOnDrop(tokio::spawn({
            let inner = inner.clone();
            let id = job.id;
            async move {
                let every = inner.config.lease / 3;
                let lease = inner.config.lease.as_secs_f64();
                loop {
                    tokio::time::sleep(every).await;
                    match queue::heartbeat(&inner.pool, id, &inner.id, lease).await {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(%id, "job lease was taken over while still running"),
                        Err(error) => tracing::warn!(%id, %error, "job heartbeat failed"),
                    }
                }
            }
        }));
        let cx =
            Context { id: job.id, attempt: u32::try_from(job.attempts).unwrap_or(u32::MAX), pool: inner.pool.clone() };
        let run = (handler.run)(job.payload.clone(), inner.state.clone(), cx);
        let outcome = match tokio::time::timeout(handler.timeout, run).await {
            Ok(result) => result,
            Err(_) => Err(JobError::Retry(anyhow::anyhow!("timed out after {:?}", handler.timeout))),
        };
        drop(heartbeat);
        record(&inner, &job, outcome).await;
    }
    .instrument(span)
    .await
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn record<S>(inner: &Inner<S>, job: &queue::Claimed, outcome: Result<(), JobError>) {
    let result = match outcome {
        Ok(()) => {
            tracing::info!("job done");
            queue::complete(&inner.pool, job.id, &inner.id).await
        }
        Err(JobError::Retry(error)) if job.attempts < job.max_attempts => {
            let delay = backoff(job.attempts);
            tracing::warn!(error = %format!("{error:#}"), delay_secs = delay.as_secs(), "job failed; will retry");
            queue::retry_later(&inner.pool, job.id, &inner.id, delay.as_secs_f64(), &truncate(&error)).await
        }
        Err(error) => {
            tracing::error!(error = %error, attempts = job.attempts, "job dead");
            let message = match error {
                JobError::Retry(e) => truncate(&e),
                JobError::Discard(e) => format!("discarded: {}", truncate(&e)),
            };
            queue::mark_dead(&inner.pool, job.id, &inner.id, &message).await
        }
    };
    match result {
        Ok(true) => {}
        Ok(false) => tracing::warn!("job finished after its lease was taken over; another worker owns the result"),
        Err(error) => tracing::error!(%error, "recording the job outcome failed; lease expiry will rerun it"),
    }
}

/// `base * 2^(attempt-1)`, capped, plus up to 10% jitter so retries from one
/// outage do not land on the provider in lockstep.
fn backoff(attempt: i32) -> Duration {
    let exponent = u32::try_from(attempt.max(1) - 1).unwrap_or(u32::MAX).min(20);
    let base = BACKOFF_BASE.saturating_mul(2u32.saturating_pow(exponent)).min(BACKOFF_CAP);
    let mut random = [0u8; 2];
    getrandom::fill(&mut random).expect("OS randomness");
    let jitter = f64::from(u16::from_le_bytes(random)) / f64::from(u16::MAX) * 0.1;
    base.mul_f64(1.0 + jitter)
}

fn truncate(error: &anyhow::Error) -> String {
    let mut text = format!("{error:#}");
    if text.len() > MAX_ERROR_LEN {
        let mut end = MAX_ERROR_LEN;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::{Enqueue, Job, Role, enqueue, retry_dead, stats};
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    #[derive(Default)]
    struct Observed {
        runs: AtomicUsize,
        ids: Mutex<HashSet<Uuid>>,
        attempts: Mutex<Vec<u32>>,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    type State = Arc<Observed>;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Count {
        n: u32,
    }
    impl Job for Count {
        const KIND: &'static str = "count";
        type State = State;
        async fn run(self, state: State, cx: Context) -> Result<(), JobError> {
            state.runs.fetch_add(1, Ordering::SeqCst);
            assert!(state.ids.lock().unwrap().insert(cx.id), "job {} ran twice", cx.id);
            Ok(())
        }
    }

    /// Fails `fail` times, then succeeds. Records the attempt numbers it saw.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Flaky {
        fail: u32,
        #[serde(default)]
        permanent: bool,
    }
    impl Job for Flaky {
        const KIND: &'static str = "flaky";
        const MAX_ATTEMPTS: u32 = 3;
        type State = State;
        async fn run(self, state: State, cx: Context) -> Result<(), JobError> {
            state.attempts.lock().unwrap().push(cx.attempt);
            if self.permanent {
                return Err(JobError::discard(anyhow::anyhow!("bad input")));
            }
            if cx.attempt <= self.fail {
                return Err(anyhow::anyhow!("transient failure #{}", cx.attempt).into());
            }
            Ok(())
        }
    }

    /// Sleeps `ms` on its first attempt; completes immediately after.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Slow {
        ms: u64,
    }
    impl Job for Slow {
        const KIND: &'static str = "slow";
        const TIMEOUT: Duration = Duration::from_millis(300);
        const MAX_ATTEMPTS: u32 = 2;
        type State = State;
        async fn run(self, state: State, cx: Context) -> Result<(), JobError> {
            state.runs.fetch_add(1, Ordering::SeqCst);
            state.attempts.lock().unwrap().push(cx.attempt);
            state.started.notify_one();
            if cx.attempt == 1 {
                tokio::time::sleep(Duration::from_millis(self.ms)).await;
            }
            Ok(())
        }
    }

    /// Hold the original attempt until the test has completed the ownership transfer.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Held;
    impl Job for Held {
        const KIND: &'static str = "held";
        const TIMEOUT: Duration = Duration::from_secs(60);
        type State = State;
        async fn run(self, state: State, cx: Context) -> Result<(), JobError> {
            state.runs.fetch_add(1, Ordering::SeqCst);
            state.attempts.lock().unwrap().push(cx.attempt);
            state.started.notify_one();
            if cx.attempt == 1 {
                state.release.notified().await;
            }
            Ok(())
        }
    }

    fn config(lease_ms: u64, concurrency: usize) -> Config {
        Config {
            role: Role::Both,
            concurrency,
            poll: Duration::from_millis(50),
            shutdown: Duration::from_secs(5),
            lease: Duration::from_millis(lease_ms),
        }
    }

    fn registry() -> Registry<State> {
        Registry::new().register::<Count>().register::<Flaky>().register::<Slow>().register::<Held>()
    }

    struct Running {
        handle: tokio::task::JoinHandle<()>,
        shutdown: watch::Sender<bool>,
    }
    fn start(pool: &Pool, state: &State, config: Config) -> Running {
        let (shutdown, rx) = watch::channel(false);
        let worker = Worker::new(pool.clone(), state.clone(), registry(), config);
        Running { handle: tokio::spawn(worker.run(rx)), shutdown }
    }
    impl Running {
        async fn stop(self) {
            self.shutdown.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(10), self.handle).await.expect("worker stops").unwrap();
        }
    }

    async fn count_state(pool: &Pool, state: &str) -> i64 {
        sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM jobs WHERE state = $1"#, state)
            .fetch_one(pool)
            .await
            .unwrap()
    }
    async fn wait_until(pool: &Pool, state: &str, n: i64) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while count_state(pool, state).await != n {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {n} jobs in state {state}"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn every_job_runs_exactly_once_across_concurrent_workers(pool: Pool) {
        let state = State::default();
        for n in 0..200 {
            enqueue(&pool, &Count { n }, Enqueue::default()).await.unwrap().unwrap();
        }
        let workers: Vec<_> = (0..4).map(|_| start(&pool, &state, config(60_000, 3))).collect();
        wait_until(&pool, "done", 200).await;
        for worker in workers {
            worker.stop().await;
        }
        assert_eq!(state.runs.load(Ordering::SeqCst), 200);
        assert_eq!(state.ids.lock().unwrap().len(), 200);
        let unlocked =
            sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM jobs WHERE locked_by IS NULL AND attempts = 1"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unlocked, 200);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn enqueue_inside_a_rolled_back_transaction_leaves_nothing(pool: Pool) {
        let mut tx = pool.begin().await.unwrap();
        enqueue(&mut *tx, &Count { n: 1 }, Enqueue::default()).await.unwrap().unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(count_state(&pool, "queued").await, 0);

        let mut tx = pool.begin().await.unwrap();
        enqueue(&mut *tx, &Count { n: 2 }, Enqueue::default()).await.unwrap().unwrap();
        // Not visible to other connections until commit.
        assert_eq!(count_state(&pool, "queued").await, 0);
        tx.commit().await.unwrap();
        assert_eq!(count_state(&pool, "queued").await, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn dedupe_key_collapses_live_duplicates_only(pool: Pool) {
        let key = Enqueue { dedupe_key: Some("user-1".into()), ..Default::default() };
        let first = enqueue(&pool, &Count { n: 1 }, key.clone()).await.unwrap();
        assert!(first.is_some());
        assert!(enqueue(&pool, &Count { n: 1 }, key.clone()).await.unwrap().is_none());
        // A different kind with the same key is a different job.
        assert!(enqueue(&pool, &Flaky { fail: 0, permanent: false }, key.clone()).await.unwrap().is_some());
        let state = State::default();
        let worker = start(&pool, &state, config(60_000, 2));
        wait_until(&pool, "done", 2).await;
        assert!(enqueue(&pool, &Count { n: 1 }, key).await.unwrap().is_some(), "finished jobs free the key");
        wait_until(&pool, "done", 3).await;
        worker.stop().await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn failures_back_off_then_die_and_can_be_retried_by_an_operator(pool: Pool) {
        let state = State::default();
        let id = enqueue(&pool, &Flaky { fail: 1, permanent: false }, Enqueue::default()).await.unwrap().unwrap();
        let dead = enqueue(&pool, &Flaky { fail: 10, permanent: false }, Enqueue::default()).await.unwrap().unwrap();
        let discarded = enqueue(&pool, &Flaky { fail: 0, permanent: true }, Enqueue::default()).await.unwrap().unwrap();
        let worker = start(&pool, &state, config(60_000, 2));
        wait_until(&pool, "done", 1).await;
        wait_until(&pool, "dead", 2).await;
        let row =
            sqlx::query!("SELECT attempts, last_error FROM jobs WHERE id = $1", id).fetch_one(&pool).await.unwrap();
        assert_eq!(row.attempts, 2);
        assert!(row.last_error.is_none());
        let row =
            sqlx::query!("SELECT attempts, last_error FROM jobs WHERE id = $1", dead).fetch_one(&pool).await.unwrap();
        assert_eq!(row.attempts, 3, "MAX_ATTEMPTS counts the first run");
        assert_eq!(row.last_error.as_deref(), Some("transient failure #3"));
        let row = sqlx::query!("SELECT attempts, last_error FROM jobs WHERE id = $1", discarded)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.attempts, 1);
        assert_eq!(row.last_error.as_deref(), Some("discarded: bad input"));
        let seen = state.attempts.lock().unwrap().clone();
        assert_eq!(seen.iter().filter(|a| **a == 1).count(), 3);

        let summary = stats(&pool).await.unwrap();
        assert!(summary.iter().any(|s| s.state == "dead" && s.kind == "flaky" && s.count == 2));
        assert!(retry_dead(&pool, dead).await.unwrap());
        assert!(!retry_dead(&pool, id).await.unwrap(), "only dead jobs can be retried");
        wait_until(&pool, "dead", 1).await;
        wait_until(&pool, "dead", 2).await;
        worker.stop().await;
        let first_delay = {
            let d = backoff(1);
            d >= Duration::from_secs(1) && d <= Duration::from_millis(1100)
        };
        assert!(first_delay);
        assert!(backoff(5) >= Duration::from_secs(16));
        assert!(backoff(100) <= BACKOFF_CAP.mul_f64(1.1));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_crashed_workers_job_is_rerun_after_its_lease_expires(pool: Pool) {
        let id = enqueue(&pool, &Count { n: 1 }, Enqueue::default()).await.unwrap().unwrap();
        // A worker that claimed the job and then vanished without heartbeats.
        let claimed = queue::claim(&pool, "crashed-worker", 0.2, &["count"]).await.unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert!(
            queue::claim(&pool, "other", 0.2, &["count"]).await.unwrap().is_none(),
            "running jobs are not reclaimed"
        );
        let state = State::default();
        let worker = start(&pool, &state, config(60_000, 1));
        wait_until(&pool, "done", 1).await;
        worker.stop().await;
        let row =
            sqlx::query!("SELECT attempts, locked_by FROM jobs WHERE id = $1", id).fetch_one(&pool).await.unwrap();
        assert_eq!(row.attempts, 2);
        assert!(row.locked_by.is_none());
        assert_eq!(state.runs.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_lost_lease_means_at_least_once_and_the_new_owner_wins(pool: Pool) {
        let state = State::default();
        let id = enqueue(&pool, &Held, Enqueue::default()).await.unwrap().unwrap();
        let worker = start(&pool, &state, config(60_000, 2));
        state.started.notified().await;
        // Simulate a stale lease: the first attempt is still running but the
        // database no longer considers this worker its owner.
        sqlx::query!(
            "UPDATE jobs SET locked_by = 'someone-else', locked_until = now() - interval '1 second' WHERE id = $1",
            id
        )
        .execute(&pool)
        .await
        .unwrap();
        wait_until(&pool, "done", 1).await;
        state.release.notify_one();
        worker.stop().await;
        assert_eq!(state.runs.load(Ordering::SeqCst), 2, "the handler ran twice");
        assert_eq!(*state.attempts.lock().unwrap(), vec![1, 2]);
        let row = sqlx::query!("SELECT attempts, state FROM jobs WHERE id = $1", id).fetch_one(&pool).await.unwrap();
        assert_eq!((row.attempts, row.state.as_str()), (2, "done"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn timeouts_cancel_the_run_and_count_as_a_retryable_failure(pool: Pool) {
        let state = State::default();
        let id = enqueue(&pool, &Slow { ms: 5_000 }, Enqueue::default()).await.unwrap().unwrap();
        let worker = start(&pool, &state, config(60_000, 1));
        wait_until(&pool, "done", 1).await;
        worker.stop().await;
        let row = sqlx::query!("SELECT attempts FROM jobs WHERE id = $1", id).fetch_one(&pool).await.unwrap();
        assert_eq!(row.attempts, 2);
        assert_eq!(*state.attempts.lock().unwrap(), vec![1, 2]);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn shutdown_drains_running_jobs_and_claims_nothing_new(pool: Pool) {
        let state = State::default();
        let running = enqueue(&pool, &Held, Enqueue::default()).await.unwrap().unwrap();
        let (shutdown, rx) = watch::channel(false);
        let worker = Worker::new(pool.clone(), state.clone(), registry(), config(60_000, 1));
        let mut handle = Box::pin(worker.run(rx));
        tokio::select! {
            _ = state.started.notified() => {}
            _ = &mut handle => panic!("worker stopped before the job started"),
        }
        let waiting = enqueue(&pool, &Count { n: 1 }, Enqueue::default()).await.unwrap().unwrap();
        shutdown.send(true).unwrap();
        // Drive shutdown while the job still holds its permit. The worker may
        // be awaiting a sweep or capacity; releasing the job must not permit a new claim.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(handle.as_mut(), cx).is_pending(), "shutdown must await the held job");
            std::task::Poll::Ready(())
        })
        .await;
        state.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap();
        let row = sqlx::query!("SELECT state FROM jobs WHERE id = $1", running).fetch_one(&pool).await.unwrap();
        assert_eq!(row.state, "done", "the in-flight job finished");
        let row = sqlx::query!("SELECT state FROM jobs WHERE id = $1", waiting).fetch_one(&pool).await.unwrap();
        assert_eq!(row.state, "queued", "nothing new was claimed after shutdown began");

        // A job that outlives the drain budget is abandoned and recovered by lease expiry.
        let state = State::default();
        let long = enqueue(&pool, &Held, Enqueue::default()).await.unwrap().unwrap();
        let (shutdown, rx) = watch::channel(false);
        let mut short = config(500, 1);
        short.shutdown = Duration::from_millis(100);
        let handle = tokio::spawn(Worker::new(pool.clone(), state.clone(), registry(), short).run(rx));
        state.started.notified().await;
        shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
        let row = sqlx::query!("SELECT state FROM jobs WHERE id = $1", long).fetch_one(&pool).await.unwrap();
        assert_eq!(row.state, "running", "abandoned until its lease expires");
        let worker = start(&pool, &state, config(60_000, 1));
        wait_until(&pool, "done", 3).await;
        worker.stop().await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn workers_only_claim_kinds_they_know_and_bad_payloads_die(pool: Pool) {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Newer;
        impl Job for Newer {
            const KIND: &'static str = "newer";
            type State = State;
            async fn run(self, state: State, _: Context) -> Result<(), JobError> {
                state.runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let state = State::default();
        let newer = enqueue(&pool, &Newer, Enqueue::default()).await.unwrap().unwrap();
        // An old binary without the `newer` handler leaves it alone.
        let old = start(&pool, &state, config(60_000, 1));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let row = sqlx::query!("SELECT state, attempts FROM jobs WHERE id = $1", newer).fetch_one(&pool).await.unwrap();
        assert_eq!((row.state.as_str(), row.attempts), ("queued", 0));
        old.stop().await;

        let bad = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO jobs (id, kind, payload, max_attempts) VALUES ($1, 'count', '{\"wrong\": true}', 5)",
            bad
        )
        .execute(&pool)
        .await
        .unwrap();
        let (shutdown, rx) = watch::channel(false);
        let registry = Registry::new().register::<Count>().register::<Newer>();
        let handle = tokio::spawn(Worker::new(pool.clone(), state.clone(), registry, config(60_000, 1)).run(rx));
        wait_until(&pool, "done", 1).await;
        wait_until(&pool, "dead", 1).await;
        shutdown.send(true).unwrap();
        handle.await.unwrap();
        let row = sqlx::query!("SELECT last_error FROM jobs WHERE id = $1", bad).fetch_one(&pool).await.unwrap();
        let error = row.last_error.unwrap();
        assert!(error.starts_with("discarded: invalid payload for count"), "{error}");
        assert!(!error.contains("wrong"), "errors never echo the payload: {error}");
        assert_eq!(state.runs.load(Ordering::SeqCst), 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn retention_purges_old_finished_rows_only(pool: Pool) {
        for (state, age) in
            [("done", "8 days"), ("done", "1 day"), ("dead", "31 days"), ("dead", "29 days"), ("queued", "40 days")]
        {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO jobs (id, kind, payload, state, max_attempts, finished_at, created_at)
                 VALUES (gen_random_uuid(), 'count', '{{}}', '{state}', 1,
                         CASE WHEN '{state}' = 'queued' THEN NULL ELSE now() - interval '{age}' END, now() - interval '{age}')"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(queue::purge_finished(&pool).await.unwrap(), 2);
        assert_eq!(count_state(&pool, "done").await, 1);
        assert_eq!(count_state(&pool, "dead").await, 1);
        assert_eq!(count_state(&pool, "queued").await, 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn delayed_jobs_wait_for_run_at(pool: Pool) {
        let state = State::default();
        let later = time::OffsetDateTime::now_utc() + time::Duration::milliseconds(400);
        enqueue(&pool, &Count { n: 1 }, Enqueue { run_at: Some(later), dedupe_key: None }).await.unwrap().unwrap();
        let worker = start(&pool, &state, config(60_000, 1));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(state.runs.load(Ordering::SeqCst), 0);
        wait_until(&pool, "done", 1).await;
        worker.stop().await;
    }

    #[test]
    fn long_errors_are_truncated_on_a_char_boundary() {
        let error = anyhow::anyhow!("{}", "é".repeat(MAX_ERROR_LEN));
        let text = truncate(&error);
        assert!(text.ends_with('…'));
        assert!(text.len() <= MAX_ERROR_LEN + '…'.len_utf8());
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_stale_worker_cannot_extend_or_overwrite_the_new_owners_claim(pool: Pool) {
        let id = enqueue(&pool, &Count { n: 1 }, Enqueue::default()).await.unwrap().unwrap();
        let first = queue::claim(&pool, "old", 60.0, &["count"]).await.unwrap().unwrap();
        assert_eq!(first.id, id);
        assert!(queue::heartbeat(&pool, id, "old", 60.0).await.unwrap());
        assert_eq!(queue::requeue_expired(&pool).await.unwrap(), 0, "a renewed lease is protected");
        // Expire the lease deterministically, without depending on wall-clock sleeps.
        assert!(queue::heartbeat(&pool, id, "old", -1.0).await.unwrap());
        assert_eq!(queue::requeue_expired(&pool).await.unwrap(), 1);
        let second = queue::claim(&pool, "new", 60.0, &["count"]).await.unwrap().unwrap();
        assert_eq!((second.id, second.attempts), (id, 2));
        assert!(!queue::heartbeat(&pool, id, "old", 60.0).await.unwrap());
        assert!(!queue::complete(&pool, id, "old").await.unwrap());
        assert!(!queue::retry_later(&pool, id, "old", 0.0, "stale retry").await.unwrap());
        assert!(!queue::mark_dead(&pool, id, "old", "stale failure").await.unwrap());
        assert_eq!(count_state(&pool, "running").await, 1);
        assert!(queue::heartbeat(&pool, id, "new", 60.0).await.unwrap());
        assert!(queue::complete(&pool, id, "new").await.unwrap());
        assert_eq!(count_state(&pool, "done").await, 1);
        assert!(!queue::heartbeat(&pool, id, "new", 60.0).await.unwrap(), "finished jobs cannot be revived");
        assert!(queue::claim(&pool, "third", 60.0, &["count"]).await.unwrap().is_none());
    }
}
