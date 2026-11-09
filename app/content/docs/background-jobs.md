+++
title = "Background jobs"
slug = "background-jobs"
description = "Durable jobs in Postgres: enqueue work from a request, run it in a worker, retry on failure."
section = "features"
order = 70
+++

## What it is

The app ships with a job queue stored in its own Postgres database. A job is
a small serializable payload with a handler. You enqueue it from a request
(ideally inside the same transaction as the data it belongs to) and a worker
runs it later, retrying with backoff if it fails. Queued jobs survive crashes,
restarts and deploys because they are rows, not memory.

Nothing else is needed: no Redis, no extension, no extra service. The worker
runs inside the web process by default and can move to its own container
with one environment variable.

## When to use which tool

- **Do it in the handler** when the user is waiting for the result: sign-in,
  form validation, anything the response needs.
- **Use `app_core::blocking::run`** for CPU-heavy work the response
  needs now (password hashing already does). It keeps request threads free
  and caps concurrent CPU work.
- **Use a job** when the work must still happen if the process dies before
  it is done, usually because it talks to another system: sending a
  non-security email, calling an API, generating a report, fanning out
  follow-up work. The user gets a fast response; the job catches up.
- **Use a Tokio interval** for cheap, idempotent periodic cleanup where a
  missed tick costs nothing (the hourly session purge does this).

Do not move security-sensitive, time-boxed flows into jobs. The signup code
email stays synchronous on purpose: the handler must know whether it was
delivered, and its budgets are the security control.

## Define a job

Jobs live in `app/src/jobs.rs`. A job is a struct that derives `Serialize`
and `Deserialize` and implements `app_core::jobs::Job`:

```rust,ignore
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SendWelcomeEmail { pub user_id: uuid::Uuid }

impl Job for SendWelcomeEmail {
    const KIND: &'static str = "send_welcome_email"; // stored in the row; never rename
    const MAX_ATTEMPTS: u32 = 5;                      // default 10
    // const TIMEOUT: Duration = Duration::from_secs(60); // default
    type State = SharedState;

    async fn run(self, state: SharedState, cx: Context) -> Result<(), JobError> {
        let Some(mail) = &state.email else { return Ok(()) };
        let Some(user) = sqlx::query!("SELECT email FROM users WHERE id = $1", self.user_id)
            .fetch_optional(&cx.pool).await? else { return Ok(()) };
        mail.send_message(&user.email, "Welcome", "...").await?; // `?` = retry later
        Ok(())
    }
}
```

Register every kind in `jobs::registry()`; a worker only claims kinds it
knows. The shipped example greets new users after email verification.
Delete it if your product does not want a welcome email; keep the pattern.

Return values decide what happens next:

- `Ok(())`: done.
- `Err(JobError::Retry(_))`, which is what `?` produces: run again after
  exponential backoff (1 s, 2 s, 4 s … capped at 10 minutes, with jitter)
  until `MAX_ATTEMPTS` runs have been used, then the job is `dead`.
- `Err(JobError::discard(_))`: wrong input, no point retrying; `dead` now.

A run longer than `TIMEOUT` is cancelled by dropping its future and counts
as a retryable failure. A panic in a job aborts the whole process in release
builds, like a panic anywhere else; return errors instead.

## Enqueue

```rust,ignore
use app_core::jobs::{enqueue, Enqueue};

let mut tx = pool.begin().await?;
// ... insert the data the job belongs to ...
enqueue(&mut *tx, &SendWelcomeEmail { user_id }, Enqueue {
    dedupe_key: Some(user_id.to_string()),
    ..Default::default()
}).await?;
tx.commit().await?;
```

Enqueueing inside the transaction means the job exists exactly when its data
does: a rollback removes both, and a worker cannot see the job before the
commit. `&pool` works too for standalone jobs. `run_at` delays a job;
`dedupe_key` makes a second enqueue a no-op while a job of the same kind with
that key is queued or running (it returns `None`).

Payloads are stored as JSON in the `jobs` table. Put identifiers in them,
not secrets or personal data; look the rest up in the handler.

## Guarantees

A job row is completed exactly once. Its handler runs **at least once**: if
the process dies after the side effect and before the row is updated, the
job runs again. Make handlers idempotent: check whether the email was already
sent, use the provider's idempotency keys, use upserts. The dedupe key stops
duplicate enqueues, not duplicate runs.

Mechanics, for the curious: a worker claims one row at a time with a single
`UPDATE … FOR UPDATE SKIP LOCKED` statement, so no two workers get the same
job. The claim is a 60-second lease extended by heartbeats while the job
runs. A worker that dies stops heartbeating; any worker's next sweep puts the
job back in the queue (its attempt still counts). Completed rows are kept
for 7 days and dead rows for 30 so you can inspect them.

## Run the worker

`JOBS_WORKER` in `.env.example` chooses the process role:

- `true` (default): serve HTTP and run jobs in the same process. Right for
  development and for most deployments.
- `false`: serve HTTP only. Use it on the web container once a separate
  worker exists.
- `only`: run jobs only and answer `/healthz`, so the container health check
  passes. The `worker` command does the same regardless of the variable.

`JOBS_CONCURRENCY` (default 2) is how many jobs run at once per process;
each may hold one database connection while it runs SQL. `JOBS_POLL_SECONDS`
(default 5) is how long a worker waits for rows when nothing in its own
process enqueued one; jobs enqueued in the same process start at once.
`JOBS_SHUTDOWN_SECONDS` (default 8) is how long running jobs get to finish
when the process stops; keep it below your container's stop grace period
(10 seconds on Docker). Jobs that outlive it are rerun elsewhere after their
lease expires.

Periodic cleanup tasks follow the worker role, so a web-only process runs
neither jobs nor cleanups.

Split the worker out when a job class is CPU- or memory-heavy enough to
compete with requests, or when you want to scale or restart it separately.
It is the same image: see [Docker hosting](/docs/docker) and
[CapRover](/docs/caprover).

## Operate

```sh
cargo run -- jobs stats          # counts per state and kind, oldest queued run_at
cargo run -- jobs retry <id>     # give a dead job a fresh set of attempts
```

Every attempt is a tracing span `job.run` with `kind`, `id` and `attempt`;
failures are warnings, dead jobs are errors, so your log or error tracking
setup sees them. `last_error` on the row holds the truncated error text.
Alert on "oldest queued job older than N minutes" if you need a queue-depth
signal.

Deploys with old and new versions running together are safe as long as a
kind is never renamed and payload fields are only added with
`#[serde(default)]`. An old worker never claims a kind it does not know.
To change a payload incompatibly, add a new kind and drain the old one.

## Tests

`app/tests/jobs.rs` runs the worker in-process against the test database
and a local mail double. To test your own job, enqueue it, start
`app_core::jobs::Worker` with a short `poll`, wait for the row to be
`done`, then flip the shutdown signal. No external services are involved.
