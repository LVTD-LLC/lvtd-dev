//! This app's background jobs. Every kind the worker can run is registered
//! in [`registry`]; the queue itself lives in `app_core::jobs`.
//!
//! When to use a job (see the Background jobs guide in `app/content/docs`):
//! work that must happen even if the process dies before it is done, that
//! talks to another system, and that the user is not waiting on. Keep what
//! the response needs in the handler, CPU work in `app_core::blocking`,
//! and idempotent hourly cleanup on a Tokio interval.
//!
//! Handlers run at least once. Make them safe to repeat.

use crate::SharedState;
#[cfg(feature = "auth")]
use crate::views::SITE_NAME;
use app_core::jobs::Registry;
#[cfg(feature = "auth")]
use app_core::jobs::{Context, Job, JobError};
#[cfg(feature = "auth")]
use uuid::Uuid;

/// Example job: greet a user after their email is verified. Enqueued inside
/// the transaction that creates the account (`verification::verify`), so an
/// account always gets exactly one queued greeting and a greeting never
/// exists without its account. Delete this job if your product does not
/// want it; keep the pattern.
#[cfg(feature = "auth")]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SendWelcomeEmail {
    pub user_id: Uuid,
}

#[cfg(feature = "auth")]
impl Job for SendWelcomeEmail {
    const KIND: &'static str = "send_welcome_email";
    const MAX_ATTEMPTS: u32 = 5;
    type State = SharedState;

    async fn run(self, state: SharedState, cx: Context) -> Result<(), JobError> {
        // No mail configuration means nothing to send; that is success, not a retry.
        let Some(mail) = &state.email else { return Ok(()) };
        // A deleted account is also a successful no-op.
        let Some(user) =
            sqlx::query!("SELECT email FROM users WHERE id = $1", self.user_id).fetch_optional(&cx.pool).await?
        else {
            return Ok(());
        };
        let text = format!(
            "Welcome to {SITE_NAME}!\n\nYour account is ready. Sign in any time at {}/sign-in.\n\nIf you did not create this account, reply to this email.",
            state.cfg.site_url
        );
        // Delivery errors are transient by default: `?` retries with backoff.
        let subject = format!("Welcome to {SITE_NAME}");
        mail.send_message(&user.email, &subject, &text).await?;
        Ok(())
    }
}

/// Remove everything a deleted account stored in Qdrant. Enqueued in the
/// transaction that deletes the user (`settings::delete`), so the purge
/// survives a crash and a Qdrant outage never blocks deleting an account.
/// Deleting by owner filter is idempotent, which retries rely on.
#[cfg(feature = "qdrant")]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PurgeVectors {
    pub user_id: Uuid,
}

#[cfg(feature = "qdrant")]
impl Job for PurgeVectors {
    const KIND: &'static str = "purge_vectors";
    type State = SharedState;

    async fn run(self, state: SharedState, _cx: Context) -> Result<(), JobError> {
        // Qdrant was never configured in this process: there is nothing to purge.
        // The worker needs the same QDRANT_* settings as the web process.
        let Some(vectors) = &state.vectors else { return Ok(()) };
        vectors.delete_all(self.user_id).await?;
        Ok(())
    }
}

/// Every job kind this binary can run. A worker only claims kinds listed
/// here, which keeps old and new versions safe to run side by side.
pub fn registry() -> Registry<SharedState> {
    let registry = Registry::new();
    #[cfg(feature = "auth")]
    let registry = registry.register::<SendWelcomeEmail>();
    #[cfg(feature = "qdrant")]
    let registry = registry.register::<PurgeVectors>();
    registry
}
