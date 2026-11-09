//! Durable background jobs stored in the app's own Postgres database.
//!
//! Use a job for work that must still happen after a crash or a deploy and
//! that the user does not need in the current response: calling a third
//! party, sending a non-security email, fanning out follow-up work. Keep
//! user-facing work in the handler, CPU work in [`crate::blocking`], and
//! idempotent periodic cleanup on a plain Tokio interval.
//!
//! Guarantees: a job row completes exactly once; its handler runs **at least
//! once**. External side effects can therefore repeat (a crash between the
//! side effect and the completion update replays the job), so handlers must
//! be idempotent: dedupe keys, provider idempotency keys, upserts.
//!
//! The DDL is the `jobs` table in each app's migrations; every query in this
//! module is compile-time checked against it. The worker runs inside the web
//! process or on its own, chosen at runtime by [`Config`].

mod queue;
mod worker;

pub use queue::{JobStats, retry_dead, stats};
pub use worker::Worker;

use crate::Pool;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

/// A unit of background work. The payload is what `enqueue` stores; the
/// handler receives it back with the app state and a [`Context`].
///
/// `KIND` is stored in every row and must never change once jobs exist:
/// a worker only claims kinds it has a handler for, which is what makes
/// deploys with old and new binaries side by side safe. Add fields with
/// `#[serde(default)]`; introduce a new kind instead of renaming one.
pub trait Job: Serialize + DeserializeOwned + Send + 'static {
    const KIND: &'static str;
    /// Attempts before the job is marked dead. The first run counts.
    const MAX_ATTEMPTS: u32 = 10;
    /// A run longer than this is cancelled (its future is dropped) and retried.
    const TIMEOUT: Duration = Duration::from_secs(60);
    /// Shared application state the handler receives (typically the app's `SharedState`).
    type State: Clone + Send + Sync + 'static;

    fn run(self, state: Self::State, cx: Context) -> impl Future<Output = Result<(), JobError>> + Send;
}

/// Why a run did not succeed. Any `?` on an error becomes `Retry`.
#[derive(Debug)]
pub enum JobError {
    /// Transient: run again after backoff until `MAX_ATTEMPTS` is reached.
    Retry(anyhow::Error),
    /// Permanent: mark the job dead now and keep the row for an operator.
    Discard(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for JobError {
    fn from(e: E) -> Self {
        JobError::Retry(e.into())
    }
}

impl JobError {
    pub fn discard(e: impl Into<anyhow::Error>) -> Self {
        JobError::Discard(e.into())
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Retry(e) => write!(f, "{e:#}"),
            JobError::Discard(e) => write!(f, "discarded: {e:#}"),
        }
    }
}

/// What the worker knows about the current attempt.
#[derive(Clone)]
pub struct Context {
    pub id: Uuid,
    /// 1 for the first run.
    pub attempt: u32,
    pub pool: Pool,
}

/// Options for [`enqueue`].
#[derive(Clone, Debug, Default)]
pub struct Enqueue {
    /// Earliest time to run; `None` means now.
    pub run_at: Option<OffsetDateTime>,
    /// Collapses duplicate enqueues: while a job of the same kind with this
    /// key is queued or running, further enqueues return `None`. A finished
    /// job frees the key, so this stops duplicate *enqueues*, not duplicate
    /// *runs*; keep handlers idempotent regardless.
    pub dedupe_key: Option<String>,
}

/// Woken by `enqueue` so a worker in this process claims the row without
/// waiting for its poll interval. Other processes see it within one poll.
static WAKE: Notify = Notify::const_new();

/// Insert a job. Works with `&pool`, `&mut *tx` or `&mut PgConnection`;
/// inside a transaction the job becomes visible to workers only on commit,
/// which is how application data and its follow-up work stay atomic.
///
/// Returns the new id, or `None` when a `dedupe_key` matched a live job.
pub async fn enqueue<'e, J: Job>(
    executor: impl sqlx::PgExecutor<'e>,
    job: &J,
    options: Enqueue,
) -> anyhow::Result<Option<Uuid>> {
    let payload = serde_json::to_string(job)?;
    let max_attempts = i32::try_from(J::MAX_ATTEMPTS.max(1))?;
    let id = queue::insert(executor, Uuid::now_v7(), J::KIND, &payload, options, max_attempts).await?;
    if id.is_some() {
        WAKE.notify_one();
    }
    Ok(id)
}

type BoxFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send>>;
type Runner<S> = Arc<dyn Fn(String, S, Context) -> BoxFuture + Send + Sync>;

pub(crate) struct Handler<S> {
    pub(crate) timeout: Duration,
    pub(crate) run: Runner<S>,
}

/// The job kinds a worker can run, built once at startup by the app.
pub struct Registry<S> {
    handlers: HashMap<&'static str, Handler<S>>,
}

impl<S: Clone + Send + Sync + 'static> Default for Registry<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Clone + Send + Sync + 'static> Registry<S> {
    pub fn new() -> Self {
        Self { handlers: HashMap::new() }
    }

    /// Registering the same kind twice is a programming error and panics at startup.
    pub fn register<J: Job<State = S>>(mut self) -> Self {
        let run: Runner<S> = Arc::new(|payload, state, cx| {
            Box::pin(async move {
                // serde_json errors report positions, never the document itself.
                let job: J = serde_json::from_str(&payload)
                    .map_err(|e| JobError::Discard(anyhow::anyhow!("invalid payload for {}: {e}", J::KIND)))?;
                job.run(state, cx).await
            })
        });
        let previous = self.handlers.insert(J::KIND, Handler { timeout: J::TIMEOUT, run });
        assert!(previous.is_none(), "job kind {:?} registered twice", J::KIND);
        self
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<_> = self.handlers.keys().copied().collect();
        kinds.sort_unstable();
        kinds
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    pub(crate) fn get(&self, kind: &str) -> Option<&Handler<S>> {
        self.handlers.get(kind)
    }
}

/// Which parts of the binary a process runs. Set by `JOBS_WORKER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Serve HTTP and run the worker (default; one container does everything).
    Both,
    /// Serve HTTP only; a separate worker process claims jobs.
    Serve,
    /// Run the worker only, plus a health endpoint for the container check.
    Worker,
}

impl Role {
    pub fn runs_worker(self) -> bool {
        self != Role::Serve
    }

    pub fn serves_http(self) -> bool {
        self != Role::Worker
    }
}

/// Runtime configuration for the worker, read once from the environment.
/// Every variable is documented in `.env.example`.
#[derive(Clone, Debug)]
pub struct Config {
    pub role: Role,
    /// Jobs running at once in this process.
    pub concurrency: usize,
    /// How long to wait for new rows when nothing woke the worker.
    pub poll: Duration,
    /// Drain budget at shutdown; keep it below the container's stop grace period.
    pub shutdown: Duration,
    /// How long a claim is valid without a heartbeat. Fixed; exposed for tests.
    pub lease: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `get` instead of the process
    /// environment, so tests never have to mutate it.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let get = |name: &str| get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let role = match get("JOBS_WORKER").as_deref() {
            None | Some("true") => Role::Both,
            Some("false") => Role::Serve,
            Some("only") => Role::Worker,
            Some(other) => anyhow::bail!("JOBS_WORKER must be true, false or only (got {other:?})"),
        };
        let number = |name: &str, default: u64| -> anyhow::Result<u64> {
            match get(name) {
                None => Ok(default),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow::anyhow!("{name} must be a positive whole number (got {v:?})")),
            }
        };
        Ok(Self {
            role,
            concurrency: usize::try_from(number("JOBS_CONCURRENCY", 2)?)?,
            poll: Duration::from_secs(number("JOBS_POLL_SECONDS", 5)?),
            shutdown: Duration::from_secs(number("JOBS_SHUTDOWN_SECONDS", 8)?),
            lease: Duration::from_secs(60),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_validation() {
        let parse = |vars: &[(&str, &str)]| {
            Config::parse(|name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
        };
        let config = parse(&[]).unwrap();
        assert_eq!(config.role, Role::Both);
        assert_eq!(config.concurrency, 2);
        assert_eq!(config.poll, Duration::from_secs(5));
        assert_eq!(config.shutdown, Duration::from_secs(8));
        assert!(config.role.runs_worker() && config.role.serves_http());

        let config = parse(&[("JOBS_WORKER", "only"), ("JOBS_CONCURRENCY", " 4 ")]).unwrap();
        assert_eq!(config.role, Role::Worker);
        assert!(config.role.runs_worker() && !config.role.serves_http());
        assert_eq!(config.concurrency, 4);

        assert_eq!(parse(&[("JOBS_WORKER", "false")]).unwrap().role, Role::Serve);
        assert_eq!(parse(&[("JOBS_WORKER", " ")]).unwrap().role, Role::Both);
        assert!(parse(&[("JOBS_WORKER", "yes")]).is_err());
        assert!(parse(&[("JOBS_WORKER", "true"), ("JOBS_CONCURRENCY", "0")]).is_err());
    }

    #[test]
    fn registry_lists_kinds_and_rejects_duplicates() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct A;
        impl Job for A {
            const KIND: &'static str = "a";
            type State = ();
            async fn run(self, _: (), _: Context) -> Result<(), JobError> {
                Ok(())
            }
        }
        let registry = Registry::<()>::new().register::<A>();
        assert_eq!(registry.kinds(), ["a"]);
        assert!(std::panic::catch_unwind(|| Registry::<()>::new().register::<A>().register::<A>()).is_err());
    }

    #[test]
    fn errors_default_to_retry_and_display_their_cause() {
        let retry: JobError = anyhow::anyhow!("provider down").into();
        assert!(matches!(retry, JobError::Retry(_)));
        assert_eq!(retry.to_string(), "provider down");
        assert_eq!(JobError::discard(anyhow::anyhow!("bad input")).to_string(), "discarded: bad input");
    }
}
