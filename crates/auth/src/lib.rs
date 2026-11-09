//! auth: email + password accounts and cookie sessions.
//!
//! - [`password`] Argon2id hashing, run on the blocking pool
//! - [`users`]    the `users` table
//! - [`sessions`] the `sessions` table + the `session` cookie
//! - [`extract`]  `CurrentUser` / `MaybeUser` axum extractors
//! - [`webauthn`] passkey (WebAuthn) verification, pure
//! - [`passkeys`] the `passkeys` table, challenges and the two ceremonies
//! - [`totp`]     RFC 6238 one-time passwords, pure
//! - [`two_factor`] TOTP secrets, recovery codes and the sign-in challenge
//! - [`oauth_server`]    an OAuth 2.1 authorization server for the API and MCP server
//!
//! This crate owns the logic and the SQL. It does not own any HTML: the app
//! renders sign-in / sign-up forms and calls into here, so the pages can be
//! restyled freely without touching auth.

/// This crate's tracing target, independent of the dependency alias.
pub const LOG_TARGET: &str = env!("CARGO_CRATE_NAME");

pub mod extract;
pub mod oauth_server;
pub mod passkeys;
pub mod password;
pub mod sessions;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod totp;
pub mod two_factor;
pub mod users;
pub mod webauthn;

pub use extract::{AuthState, CurrentUser, MaybeUser};
pub use sessions::{Session, clear_cookie, session_cookie};
pub use users::{SignUpError, User};
pub use webauthn::RelyingParty;

/// `n` cryptographically random bytes, hex-encoded (`2n` characters).
pub(crate) fn random_hex(n: usize) -> anyhow::Result<String> {
    let mut raw = vec![0u8; n];
    getrandom::fill(&mut raw)?;
    Ok(hex(&raw))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_two_chars_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn random_hex_is_unique_and_sized() {
        let a = random_hex(32).unwrap();
        let b = random_hex(32).unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
