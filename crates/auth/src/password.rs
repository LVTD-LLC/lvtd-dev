//! Argon2id password hashing. Hashing takes tens of milliseconds by design,
//! so it always runs on tokio's blocking pool, never on a request thread.

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use std::sync::LazyLock;

pub const MIN_LEN: usize = 8;
pub const MAX_LEN: usize = 256;

/// Hash of an unguessable password. Verified against on sign-in when the
/// email is unknown, so "no such user" takes as long as "wrong password".
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_sync(&crate::random_hex(32).expect("OS randomness")).expect("argon2 hashing works"));

fn hash_sync(password: &str) -> anyhow::Result<String> {
    let hash = Argon2::default().hash_password(password.as_bytes()).map_err(anyhow::Error::msg)?;
    Ok(hash.to_string())
}

pub async fn hash(password: String) -> anyhow::Result<String> {
    app_core::blocking::run(move || hash_sync(&password)).await?
}

/// A missing hash still performs verification to avoid revealing unknown emails.
pub async fn verify(password: String, hash: Option<String>) -> anyhow::Result<bool> {
    app_core::blocking::run(move || {
        // Lazy initialization (and waiting for it) must also stay off async workers.
        let hash = hash.as_deref().unwrap_or_else(|| DUMMY_HASH.as_str());
        match Argon2::default().verify_password(password.as_bytes(), hash) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
            Err(e) => Err(anyhow::Error::msg(e)),
        }
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip() {
        let h = hash("correct-horse".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct-horse".into(), Some(h.clone())).await.unwrap());
        assert!(!verify("wrong-horse".into(), Some(h)).await.unwrap());
    }

    #[test]
    fn cold_unknown_passwords_yield_before_initializing_dummy_hash() {
        use std::{future::Future, task::Poll};

        let runtime = tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).build().unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::channel::<()>();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = wait.recv();
            });
            ready.await.unwrap();

            // With the blocking pool occupied, neither first request may initialize
            // the hash or wait synchronously for another request to initialize it.
            assert!(LazyLock::get(&DUMMY_HASH).is_none());
            let mut checks = std::pin::pin!(async {
                tokio::join!(verify("anything".into(), None), verify("something-else".into(), None))
            });
            std::future::poll_fn(|cx| {
                assert!(checks.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert!(LazyLock::get(&DUMMY_HASH).is_none());

            drop(release);
            blocker.await.unwrap();
            let (first, second) = checks.await;
            assert!(!first.unwrap());
            assert!(!second.unwrap());
            assert!(LazyLock::get(&DUMMY_HASH).is_some());
        });
    }
}
