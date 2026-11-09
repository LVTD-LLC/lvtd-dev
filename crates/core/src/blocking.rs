//! A process-wide budget for short CPU-intensive jobs.

use tokio::sync::Semaphore;
use tokio::task::JoinError;

// Two concurrent jobs keep Argon2 memory use around 38 MiB at its defaults,
// while sharing the CPU budget with other expensive work in the application.
static CPU_JOBS: Semaphore = Semaphore::const_new(2);

/// Wait asynchronously for capacity, then run on the blocking pool.
/// Cancellation while waiting submits no work. After submission, capacity stays
/// with the closure until it finishes, even if the caller times out or is dropped.
pub async fn run<F, R>(job: F) -> Result<R, JoinError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    run_with(&CPU_JOBS, job).await
}

async fn run_with<F, R>(capacity: &'static Semaphore, job: F) -> Result<R, JoinError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let permit = capacity.acquire().await.expect("CPU job semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Future, poll_fn};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;

    #[tokio::test]
    async fn cancelled_waiter_never_submits_work() {
        static CAPACITY: Semaphore = Semaphore::const_new(1);
        static RAN: AtomicBool = AtomicBool::new(false);
        let permit = CAPACITY.acquire().await.unwrap();
        let mut waiting = Box::pin(run_with(&CAPACITY, || RAN.store(true, Ordering::SeqCst)));
        poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(waiting);
        drop(permit);
        run_with(&CAPACITY, || ()).await.unwrap();
        assert!(!RAN.load(Ordering::SeqCst));
        assert_eq!(CAPACITY.available_permits(), 1);
    }

    #[tokio::test]
    async fn cancelled_caller_retains_capacity_until_job_finishes() {
        static CAPACITY: Semaphore = Semaphore::const_new(1);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let caller = tokio::spawn(run_with(&CAPACITY, move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        }));
        ready.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(CAPACITY.available_permits(), 0);

        let mut next = Box::pin(run_with(&CAPACITY, || 42));
        poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(release);
        assert_eq!(next.await.unwrap(), 42);
        assert_eq!(CAPACITY.available_permits(), 1);
    }

    #[tokio::test]
    async fn panicking_job_releases_capacity() {
        static CAPACITY: Semaphore = Semaphore::const_new(1);
        assert!(run_with(&CAPACITY, || panic!("test panic")).await.unwrap_err().is_panic());
        assert_eq!(CAPACITY.available_permits(), 1);
        run_with(&CAPACITY, || ()).await.unwrap();
    }
}
