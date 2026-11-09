//! Two-factor authentication: a TOTP secret per account, single-use recovery
//! codes, and the challenge a sign-in waits in between the password and
//! the code.
//!
//! Enabling is two steps from account settings: [`begin`] issues a secret
//! (shown as a QR code and a manual key) and [`confirm`] turns it on once a
//! code from the app has been verified, handing back recovery codes that
//! exist nowhere else. Signing in with the password then creates a
//! [`start_challenge`] instead of a session; [`finish_challenge`] trades a
//! correct code (or an unused recovery code) for the user. Passkeys are
//! already two factors, so passkey sign-in does not come through here.

use crate::totp;
use crate::users::User;
use app_core::Pool;
use axum::http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

pub const RECOVERY_CODE_COUNT: usize = 8;
/// Wrong codes a sign-in challenge tolerates before it is discarded.
pub const MAX_ATTEMPTS: i32 = 5;
pub const CHALLENGE_TTL_SECONDS: i64 = 10 * 60;
pub const COOKIE_NAME: &str = "two_factor";

/// Lower-case letters and digits without the ones that are misread
/// (`i`, `l`, `o`, `0`, `1`); ten of them are about 50 bits.
const RECOVERY_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
const RECOVERY_CODE_LEN: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum TwoFactorError {
    #[error("Two-factor authentication is already on.")]
    AlreadyEnabled,
    #[error("Two-factor authentication is not being set up. Start again.")]
    NotPending,
    #[error("Two-factor authentication is not on.")]
    NotEnabled,
    #[error("That code is not correct. Enter the current six-digit code from your authenticator app.")]
    WrongCode,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for TwoFactorError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChallengeError {
    /// No live challenge for this browser: the sign-in must start over.
    #[error("Your sign-in has expired. Enter your email and password again.")]
    NoChallenge,
    #[error("That code is not correct.")]
    WrongCode { attempts_left: i32 },
    /// The challenge has been discarded; the password is required again.
    #[error("Too many incorrect codes. Enter your email and password again.")]
    TooManyAttempts,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ChallengeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Off,
    /// A secret was issued but no code has been verified yet. Sign-in does
    /// not ask for a code; the user is still scanning the QR code.
    Pending,
    On {
        recovery_codes_left: i64,
    },
}

impl Status {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On { .. })
    }
}

/// What the enrolment page shows. The secret leaves the server only here.
#[derive(Clone, Debug)]
pub struct Setup {
    pub secret: Vec<u8>,
    /// The secret in base32, in groups of four for typing into an app.
    pub manual_key: String,
    pub otpauth_url: String,
    /// The `otpauth` URL as an inline SVG QR code (black on white, with a quiet zone).
    pub qr_svg: String,
}

impl Setup {
    fn new(secret: Vec<u8>, issuer: &str, account: &str) -> anyhow::Result<Self> {
        let otpauth_url = totp::otpauth_url(issuer, account, &secret);
        let qr_svg = qr_svg(&otpauth_url)?;
        let base32 = totp::base32_encode(&secret);
        let manual_key = base32
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Self { secret, manual_key, otpauth_url, qr_svg })
    }
}

pub async fn status(pool: &Pool, user_id: Uuid) -> anyhow::Result<Status> {
    let row = sqlx::query!(
        r#"SELECT t.confirmed_at,
                  (SELECT count(*) FROM recovery_codes r WHERE r.user_id = t.user_id AND r.used_at IS NULL) AS "codes_left!"
           FROM totp_secrets t WHERE t.user_id = $1"#,
        user_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => Status::Off,
        Some(row) if row.confirmed_at.is_none() => Status::Pending,
        Some(row) => Status::On { recovery_codes_left: row.codes_left },
    })
}

pub async fn is_enabled(pool: &Pool, user_id: Uuid) -> anyhow::Result<bool> {
    Ok(sqlx::query!(
        r#"SELECT EXISTS(SELECT 1 FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NOT NULL) AS "enabled!""#,
        user_id
    )
    .fetch_one(pool)
    .await?
    .enabled)
}

/// Issue a new secret to scan. Replaces an unconfirmed one; refuses to touch
/// a confirmed one (disable first).
pub async fn begin(pool: &Pool, user_id: Uuid, issuer: &str, account: &str) -> Result<Setup, TwoFactorError> {
    let secret = totp::generate_secret()?;
    let row = sqlx::query!(
        r#"INSERT INTO totp_secrets (user_id, secret) VALUES ($1, $2)
           ON CONFLICT (user_id) DO UPDATE SET secret = EXCLUDED.secret, created_at = now(), last_used_step = 0
           WHERE totp_secrets.confirmed_at IS NULL
           RETURNING secret"#,
        user_id,
        secret
    )
    .fetch_optional(pool)
    .await?;
    match row {
        Some(row) => Ok(Setup::new(row.secret, issuer, account)?),
        None => Err(TwoFactorError::AlreadyEnabled),
    }
}

/// The enrolment in progress, if any, for rendering the page again.
pub async fn pending(pool: &Pool, user_id: Uuid, issuer: &str, account: &str) -> anyhow::Result<Option<Setup>> {
    let row = sqlx::query!("SELECT secret FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NULL", user_id)
        .fetch_optional(pool)
        .await?;
    row.map(|row| Setup::new(row.secret, issuer, account)).transpose()
}

/// Drop an unconfirmed secret. `false` when there was none.
pub async fn cancel(pool: &Pool, user_id: Uuid) -> anyhow::Result<bool> {
    let deleted = sqlx::query!("DELETE FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NULL", user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(deleted == 1)
}

/// Turn the pending secret on by proving the app produces its codes, and
/// hand back the recovery codes. The plaintext codes are not stored.
pub async fn confirm(
    pool: &Pool,
    user_id: Uuid,
    code: &str,
    now: OffsetDateTime,
) -> Result<Vec<String>, TwoFactorError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "SELECT secret, last_used_step FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NULL FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(TwoFactorError::NotPending)?;
    let step = totp::normalize_code(code)
        .and_then(|code| totp::matching_step(&row.secret, &code, totp::step_at(now), row.last_used_step))
        .ok_or(TwoFactorError::WrongCode)?;
    sqlx::query!("UPDATE totp_secrets SET confirmed_at = now(), last_used_step = $2 WHERE user_id = $1", user_id, step)
        .execute(&mut *tx)
        .await?;
    let codes = replace_recovery_codes(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(codes)
}

/// Turn two-factor authentication off and forget the secret, the recovery
/// codes and any sign-in waiting for a code. `false` when it was not on.
pub async fn disable(pool: &Pool, user_id: Uuid) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query!("DELETE FROM totp_secrets WHERE user_id = $1 RETURNING confirmed_at", user_id)
        .fetch_optional(&mut *tx)
        .await?;
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user_id).execute(&mut *tx).await?;
    sqlx::query!("DELETE FROM two_factor_challenges WHERE user_id = $1", user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(deleted.is_some_and(|row| row.confirmed_at.is_some()))
}

/// Replace every recovery code, used or not, with a fresh set.
pub async fn regenerate_recovery_codes(pool: &Pool, user_id: Uuid) -> Result<Vec<String>, TwoFactorError> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "SELECT user_id FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NOT NULL FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(TwoFactorError::NotEnabled)?;
    let codes = replace_recovery_codes(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(codes)
}

async fn replace_recovery_codes(tx: &mut PgConnection, user_id: Uuid) -> anyhow::Result<Vec<String>> {
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user_id).execute(&mut *tx).await?;
    let mut codes = Vec::with_capacity(RECOVERY_CODE_COUNT);
    for _ in 0..RECOVERY_CODE_COUNT {
        let code = generate_recovery_code()?;
        sqlx::query!(
            "INSERT INTO recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)",
            Uuid::now_v7(),
            user_id,
            code_hash(&normalize_recovery_code(&code).expect("generated codes are well formed"))
        )
        .execute(&mut *tx)
        .await?;
        codes.push(code);
    }
    Ok(codes)
}

/// `xxxxx-xxxxx`, uniformly random over [`RECOVERY_ALPHABET`].
fn generate_recovery_code() -> anyhow::Result<String> {
    let mut code = String::with_capacity(RECOVERY_CODE_LEN + 1);
    // Rejection sampling: 248 is the largest multiple of 31 below 256.
    let limit = (u8::MAX / RECOVERY_ALPHABET.len() as u8) * RECOVERY_ALPHABET.len() as u8;
    while code.len() < RECOVERY_CODE_LEN + 1 {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        for byte in bytes {
            if code.len() == RECOVERY_CODE_LEN / 2 {
                code.push('-');
            }
            if code.len() == RECOVERY_CODE_LEN + 1 {
                break;
            }
            if byte < limit {
                code.push(RECOVERY_ALPHABET[(byte % RECOVERY_ALPHABET.len() as u8) as usize] as char);
            }
        }
    }
    Ok(code)
}

/// The user's input without separators, if it has the shape of a recovery code.
pub fn normalize_recovery_code(input: &str) -> Option<String> {
    let code: String =
        input.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_lowercase()).collect();
    (code.len() == RECOVERY_CODE_LEN && code.bytes().all(|b| RECOVERY_ALPHABET.contains(&b))).then_some(code)
}

fn code_hash(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

// ---------------------------------------------------------------------------
// The sign-in challenge
// ---------------------------------------------------------------------------

/// The password was right and the account has a second factor: remember
/// that for ten minutes and return the token for the browser's cookie.
pub async fn start_challenge(pool: &Pool, user_id: Uuid) -> anyhow::Result<String> {
    // Expired rows are swept here rather than by another background task.
    sqlx::query!("DELETE FROM two_factor_challenges WHERE expires_at <= now()").execute(pool).await?;
    let token = crate::random_hex(32)?;
    sqlx::query!(
        "INSERT INTO two_factor_challenges (token_hash, user_id, expires_at) VALUES ($1, $2, now() + make_interval(secs => $3))",
        code_hash(&token),
        user_id,
        CHALLENGE_TTL_SECONDS as f64
    )
    .execute(pool)
    .await?;
    Ok(token)
}

/// Whether the browser's token still names a challenge that can be answered.
pub async fn challenge_pending(pool: &Pool, token: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query!(
        r#"SELECT EXISTS(SELECT 1 FROM two_factor_challenges WHERE token_hash = $1 AND expires_at > now() AND attempts < $2) AS "pending!""#,
        code_hash(token),
        MAX_ATTEMPTS
    )
    .fetch_one(pool)
    .await?
    .pending)
}

/// Answer the challenge with an authenticator code or a recovery code.
/// Every call counts as an attempt; a correct answer consumes the challenge.
pub async fn finish_challenge(
    pool: &Pool,
    token: &str,
    input: &str,
    now: OffsetDateTime,
) -> Result<User, ChallengeError> {
    let token_hash = code_hash(token);
    let mut tx = pool.begin().await?;
    let challenge = sqlx::query!(
        r#"UPDATE two_factor_challenges SET attempts = attempts + 1
           WHERE token_hash = $1 AND expires_at > now()
           RETURNING user_id, attempts"#,
        token_hash
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ChallengeError::NoChallenge)?;
    let accepted = if let Some(code) = totp::normalize_code(input) {
        verify_totp(&mut tx, challenge.user_id, &code, now).await?
    } else if let Some(code) = normalize_recovery_code(input) {
        sqlx::query!(
            "UPDATE recovery_codes SET used_at = now() WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL RETURNING id",
            challenge.user_id,
            code_hash(&code)
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some()
    } else {
        false
    };
    if !accepted {
        let result = if challenge.attempts >= MAX_ATTEMPTS {
            sqlx::query!("DELETE FROM two_factor_challenges WHERE token_hash = $1", token_hash)
                .execute(&mut *tx)
                .await?;
            Err(ChallengeError::TooManyAttempts)
        } else {
            Err(ChallengeError::WrongCode { attempts_left: MAX_ATTEMPTS - challenge.attempts })
        };
        tx.commit().await?;
        return result;
    }
    sqlx::query!("DELETE FROM two_factor_challenges WHERE token_hash = $1", token_hash).execute(&mut *tx).await?;
    let user = sqlx::query_as!(
        User,
        "SELECT id, email, created_at, is_admin, avatar_key FROM users WHERE id = $1",
        challenge.user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ChallengeError::NoChallenge)?;
    tx.commit().await?;
    Ok(user)
}

/// Check a code against the confirmed secret and record the step it used,
/// so the same code is not accepted a second time.
async fn verify_totp(tx: &mut PgConnection, user_id: Uuid, code: &str, now: OffsetDateTime) -> anyhow::Result<bool> {
    let Some(row) = sqlx::query!(
        "SELECT secret, last_used_step FROM totp_secrets WHERE user_id = $1 AND confirmed_at IS NOT NULL FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    let Some(step) = totp::matching_step(&row.secret, code, totp::step_at(now), row.last_used_step) else {
        return Ok(false);
    };
    sqlx::query!("UPDATE totp_secrets SET last_used_step = $2 WHERE user_id = $1", user_id, step)
        .execute(&mut *tx)
        .await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Cookie plumbing for the challenge token
// ---------------------------------------------------------------------------

pub fn token_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|v| v.trim().strip_prefix(COOKIE_NAME).and_then(|r| r.strip_prefix('=')))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn cookie(token: &str, secure: bool) -> HeaderValue {
    let mut v = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={CHALLENGE_TTL_SECONDS}");
    if secure {
        v.push_str("; Secure");
    }
    HeaderValue::from_str(&v).expect("hex token is a valid header value")
}

pub fn clear_cookie(secure: bool) -> HeaderValue {
    let mut v = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if secure {
        v.push_str("; Secure");
    }
    HeaderValue::from_str(&v).expect("constant cookie string")
}

// ---------------------------------------------------------------------------
// QR code
// ---------------------------------------------------------------------------

/// `data` as an SVG QR code: one path of dark modules on a white square with
/// the four-module quiet zone the spec requires, in fixed colours so it scans
/// on a dark page too. Scales with CSS.
pub fn qr_svg(data: &str) -> anyhow::Result<String> {
    const QUIET: usize = 4;
    let code = qrcode::QrCode::new(data.as_bytes())?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + 2 * QUIET;
    let mut path = String::new();
    for y in 0..width {
        let mut x = 0;
        while x < width {
            if colors[y * width + x] != qrcode::Color::Dark {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && colors[y * width + x] == qrcode::Color::Dark {
                x += 1;
            }
            let run = x - start;
            path.push_str(&format!("M{} {}h{run}v1h-{run}z", start + QUIET, y + QUIET));
        }
    }
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" shape-rendering="crispEdges" role="img" aria-label="QR code to scan with your authenticator app"><rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users;

    /// The issuer shown in authenticator apps; the generator renames it with the project.
    const ISSUER: &str = "LVTD";

    #[test]
    fn recovery_codes_are_well_formed_random_and_normalized() {
        let code = generate_recovery_code().unwrap();
        assert_eq!(code.len(), RECOVERY_CODE_LEN + 1, "{code}");
        assert_eq!(code.as_bytes()[RECOVERY_CODE_LEN / 2], b'-', "{code}");
        assert!(code.bytes().filter(|b| *b != b'-').all(|b| RECOVERY_ALPHABET.contains(&b)), "{code}");
        assert_ne!(generate_recovery_code().unwrap(), code);
        let bare = code.replace('-', "");
        assert_eq!(normalize_recovery_code(&code).as_deref(), Some(bare.as_str()));
        assert_eq!(normalize_recovery_code(&format!(" {} ", bare.to_uppercase())).as_deref(), Some(bare.as_str()));
        assert_eq!(normalize_recovery_code("abcde-fghj0"), None, "0 is not in the alphabet");
        assert_eq!(normalize_recovery_code("abcde-fghjk-m"), None);
        assert_eq!(normalize_recovery_code("123456"), None, "an authenticator code is not a recovery code");
    }

    #[test]
    fn qr_svg_draws_the_finder_patterns_inside_a_quiet_zone() {
        let svg =
            qr_svg(&format!("otpauth://totp/{ISSUER}:ann%40example.com?secret=MZXW6YTBOI&issuer={ISSUER}")).unwrap();
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 "), "{svg}");
        // An otpauth URL fits a version 3 to 8 symbol (29 to 49 modules); the quiet zone adds eight.
        let size: usize = svg.split("viewBox=\"0 0 ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert!((37..=57).contains(&size), "{size}");
        assert!(svg.contains("M4 4h7v1h-7z"), "top-left finder pattern starts after the quiet zone: {svg}");
        assert!(svg.contains("fill=\"#fff\"") && svg.contains("fill=\"#000\""));
        assert_eq!(svg.matches('<').count(), 4, "svg, rect, path and the closing tag: {svg}");
    }

    #[test]
    fn cookie_is_strict_http_only_and_bounded() {
        let value = cookie("ab".repeat(32).as_str(), true);
        let value = value.to_str().unwrap();
        assert!(value.starts_with("two_factor=abab"), "{value}");
        for part in ["HttpOnly", "SameSite=Strict", "Secure", "Max-Age=600", "Path=/"] {
            assert!(value.contains(part), "{value}");
        }
        assert!(!cookie("ab", false).to_str().unwrap().contains("Secure"));
        assert!(clear_cookie(false).to_str().unwrap().contains("Max-Age=0"));
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, format!("session=x; two_factor={}", "ab".repeat(32)).parse().unwrap());
        assert_eq!(token_from_headers(&headers), Some("ab".repeat(32).as_str()));
        headers.insert(header::COOKIE, "two_factor=short".parse().unwrap());
        assert_eq!(token_from_headers(&headers), None);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn enrolment_sign_in_challenge_and_recovery_codes(pool: Pool) {
        let user = users::sign_up(&pool, "two@example.com", "correct-horse".into()).await.unwrap();
        let now = OffsetDateTime::now_utc();
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::Off);
        assert!(!is_enabled(&pool, user.id).await.unwrap());

        // Enrolment: a pending secret, replaced by a new begin, confirmed by a code.
        let first = begin(&pool, user.id, ISSUER, &user.email).await.unwrap();
        let setup = begin(&pool, user.id, ISSUER, &user.email).await.unwrap();
        assert_ne!(first.secret, setup.secret, "a new attempt gets a new secret");
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::Pending);
        assert!(!is_enabled(&pool, user.id).await.unwrap(), "pending is not on");
        assert_eq!(pending(&pool, user.id, ISSUER, &user.email).await.unwrap().unwrap().secret, setup.secret);
        assert!(setup.otpauth_url.contains(&totp::base32_encode(&setup.secret)));
        assert_eq!(setup.manual_key.replace(' ', ""), totp::base32_encode(&setup.secret));
        assert!(matches!(confirm(&pool, user.id, "000000", now).await, Err(TwoFactorError::WrongCode)));
        assert!(matches!(
            confirm(&pool, user.id, &totp::code(&first.secret, totp::step_at(now)), now).await,
            Err(TwoFactorError::WrongCode)
        ));
        let enrol_code = totp::code(&setup.secret, totp::step_at(now));
        let codes = confirm(&pool, user.id, &enrol_code, now).await.unwrap();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::On { recovery_codes_left: 8 });
        assert!(is_enabled(&pool, user.id).await.unwrap());
        assert!(matches!(confirm(&pool, user.id, &enrol_code, now).await, Err(TwoFactorError::NotPending)));
        assert!(matches!(begin(&pool, user.id, ISSUER, &user.email).await, Err(TwoFactorError::AlreadyEnabled)));
        assert!(pending(&pool, user.id, ISSUER, &user.email).await.unwrap().is_none());
        assert!(!cancel(&pool, user.id).await.unwrap(), "cancel never touches a confirmed secret");

        // Sign-in: the code used at enrolment is spent; the next step's code works once.
        let token = start_challenge(&pool, user.id).await.unwrap();
        assert!(challenge_pending(&pool, &token).await.unwrap());
        assert!(matches!(
            finish_challenge(&pool, &token, "nonsense", now).await,
            Err(ChallengeError::WrongCode { attempts_left: 4 })
        ));
        assert!(matches!(
            finish_challenge(&pool, &token, &enrol_code, now).await,
            Err(ChallengeError::WrongCode { attempts_left: 3 })
        ));
        let next_code = totp::code(&setup.secret, totp::step_at(now) + 1);
        let signed_in =
            finish_challenge(&pool, &token, &format!("{} {}", &next_code[..3], &next_code[3..]), now).await.unwrap();
        assert_eq!(signed_in.id, user.id);
        assert!(!challenge_pending(&pool, &token).await.unwrap());
        assert!(matches!(finish_challenge(&pool, &token, &next_code, now).await, Err(ChallengeError::NoChallenge)));
        let token = start_challenge(&pool, user.id).await.unwrap();
        assert!(
            matches!(finish_challenge(&pool, &token, &next_code, now).await, Err(ChallengeError::WrongCode { .. })),
            "replayed"
        );

        // A recovery code signs in exactly once; regenerating invalidates the rest.
        let recovery = codes[0].to_uppercase();
        let signed_in = finish_challenge(&pool, &token, &recovery, now).await.unwrap();
        assert_eq!(signed_in.email, "two@example.com");
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::On { recovery_codes_left: 7 });
        let token = start_challenge(&pool, user.id).await.unwrap();
        assert!(matches!(finish_challenge(&pool, &token, &recovery, now).await, Err(ChallengeError::WrongCode { .. })));
        let fresh = regenerate_recovery_codes(&pool, user.id).await.unwrap();
        assert_eq!(fresh.len(), RECOVERY_CODE_COUNT);
        assert!(fresh.iter().all(|code| !codes.contains(code)));
        assert!(matches!(finish_challenge(&pool, &token, &codes[1], now).await, Err(ChallengeError::WrongCode { .. })));
        assert_eq!(finish_challenge(&pool, &token, &fresh[7], now).await.unwrap().id, user.id);

        // Disabling forgets everything, including a challenge in flight.
        let token = start_challenge(&pool, user.id).await.unwrap();
        assert!(disable(&pool, user.id).await.unwrap());
        assert!(!disable(&pool, user.id).await.unwrap());
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::Off);
        assert!(!challenge_pending(&pool, &token).await.unwrap());
        assert!(matches!(regenerate_recovery_codes(&pool, user.id).await, Err(TwoFactorError::NotEnabled)));
        let count = sqlx::query!("SELECT count(*) AS \"n!\" FROM recovery_codes WHERE user_id = $1", user.id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .n;
        assert_eq!(count, 0);
        // A pending secret can be cancelled.
        begin(&pool, user.id, ISSUER, &user.email).await.unwrap();
        assert!(cancel(&pool, user.id).await.unwrap());
        assert_eq!(status(&pool, user.id).await.unwrap(), Status::Off);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn challenges_lock_after_five_wrong_codes_and_expire(pool: Pool) {
        let user = users::sign_up(&pool, "lock@example.com", "correct-horse".into()).await.unwrap();
        let now = OffsetDateTime::now_utc();
        let setup = begin(&pool, user.id, ISSUER, &user.email).await.unwrap();
        confirm(&pool, user.id, &totp::code(&setup.secret, totp::step_at(now)), now).await.unwrap();

        let token = start_challenge(&pool, user.id).await.unwrap();
        for attempts_left in (1..MAX_ATTEMPTS).rev() {
            match finish_challenge(&pool, &token, "000000", now).await {
                Err(ChallengeError::WrongCode { attempts_left: left }) => assert_eq!(left, attempts_left),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(finish_challenge(&pool, &token, "000000", now).await, Err(ChallengeError::TooManyAttempts)));
        let right = totp::code(&setup.secret, totp::step_at(now) + 1);
        assert!(matches!(finish_challenge(&pool, &token, &right, now).await, Err(ChallengeError::NoChallenge)));
        assert!(!challenge_pending(&pool, &token).await.unwrap());

        let expired = start_challenge(&pool, user.id).await.unwrap();
        sqlx::query!("UPDATE two_factor_challenges SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!challenge_pending(&pool, &expired).await.unwrap());
        assert!(matches!(finish_challenge(&pool, &expired, &right, now).await, Err(ChallengeError::NoChallenge)));
        // Issuing a challenge sweeps expired rows; a token is never guessable from another.
        let live = start_challenge(&pool, user.id).await.unwrap();
        assert_ne!(live, expired);
        let rows =
            sqlx::query!("SELECT count(*) AS \"n!\" FROM two_factor_challenges").fetch_one(&pool).await.unwrap().n;
        assert_eq!(rows, 1);
        assert!(matches!(
            finish_challenge(&pool, "0".repeat(64).as_str(), &right, now).await,
            Err(ChallengeError::NoChallenge)
        ));
    }
}
