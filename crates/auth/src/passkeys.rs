//! Passkeys: WebAuthn discoverable credentials attached to an account.
//!
//! A passkey is never a way to create an account. It is added from account
//! settings by a signed-in user who just confirmed their password, and it
//! then signs that user in without a password. Challenges live in Postgres
//! so they are single-use and work across replicas; the protocol checks are
//! in [`crate::webauthn`].

use crate::users::User;
use crate::webauthn::{self, ALGORITHMS, Rejected, RelyingParty, b64url_decode, b64url_encode};
use app_core::Pool;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

/// Enough for every device a person owns; keeps `excludeCredentials` small.
pub const MAX_PER_USER: i64 = 10;
pub const MAX_NAME_LEN: usize = 64;
const TRANSPORTS: [&str; 6] = ["usb", "nfc", "ble", "internal", "hybrid", "smart-card"];

#[derive(Debug, thiserror::Error)]
pub enum PasskeyError {
    /// The client's data was unacceptable; the message can be shown as-is.
    #[error("{0}")]
    Rejected(#[from] Rejected),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for PasskeyError {
    fn from(error: sqlx::Error) -> Self {
        Self::Other(error.into())
    }
}

/// What the account settings page shows. Never includes key material.
#[derive(Clone, Debug)]
pub struct Passkey {
    pub id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    /// Synced to the user's other devices (iCloud Keychain, Google Password Manager, ...).
    pub backed_up: bool,
    pub transports: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct RegistrationResponse {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub response: AttestationResponse,
}

#[derive(Debug, serde::Deserialize)]
pub struct AttestationResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
    #[serde(default)]
    pub transports: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct AuthenticationResponse {
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub response: AssertionResponse,
}

#[derive(Debug, serde::Deserialize)]
pub struct AssertionResponse {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    pub signature: String,
    #[serde(default, rename = "userHandle")]
    pub user_handle: Option<String>,
}

pub async fn list(pool: &Pool, user_id: Uuid) -> anyhow::Result<Vec<Passkey>> {
    Ok(sqlx::query_as!(
        Passkey,
        r#"SELECT id, name, created_at, last_used_at, backed_up, transports
           FROM passkeys WHERE user_id = $1 ORDER BY created_at, id"#,
        user_id
    )
    .fetch_all(pool)
    .await?)
}

/// Remove one of the user's passkeys. `false` when it was not theirs.
pub async fn remove(pool: &Pool, user_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
    let deleted = sqlx::query!("DELETE FROM passkeys WHERE id = $1 AND user_id = $2", id, user_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(deleted == 1)
}

async fn issue_challenge(pool: &Pool, user_id: Option<Uuid>, purpose: &str) -> anyhow::Result<Vec<u8>> {
    let mut challenge = vec![0u8; 32];
    getrandom::fill(&mut challenge)?;
    // Expired rows are swept here rather than by another background task.
    sqlx::query!("DELETE FROM passkey_challenges WHERE expires_at <= now()").execute(pool).await?;
    sqlx::query!(
        r#"INSERT INTO passkey_challenges (challenge, user_id, purpose, expires_at)
           VALUES ($1, $2, $3, now() + interval '10 minutes')"#,
        challenge,
        user_id,
        purpose
    )
    .execute(pool)
    .await?;
    Ok(challenge)
}

/// Single use: the row is gone whether or not the ceremony then succeeds.
async fn consume_challenge(
    pool: &Pool,
    challenge: &[u8],
    user_id: Option<Uuid>,
    purpose: &str,
) -> anyhow::Result<bool> {
    let row = sqlx::query!(
        r#"DELETE FROM passkey_challenges
           WHERE challenge = $1 AND purpose = $2 AND user_id IS NOT DISTINCT FROM $3 AND expires_at > now()
           RETURNING challenge"#,
        challenge,
        purpose,
        user_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

fn challenge_of(client_data_json: &[u8]) -> Result<Vec<u8>, Rejected> {
    let challenge = webauthn::client_data_challenge(client_data_json)?;
    if challenge.len() != 32 {
        return Err(Rejected("Malformed challenge."));
    }
    Ok(challenge)
}

fn normalize_name(raw: &str) -> Result<String, Rejected> {
    let name = raw.trim();
    if name.is_empty() {
        return Ok("Passkey".into());
    }
    if name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control) {
        return Err(Rejected("Use a passkey name of 64 characters or fewer."));
    }
    Ok(name.to_string())
}

fn normalize_transports(raw: &[String]) -> Vec<String> {
    let mut transports: Vec<String> =
        raw.iter().filter(|t| TRANSPORTS.contains(&t.as_str())).map(|t| t.to_string()).collect();
    transports.sort();
    transports.dedup();
    transports
}

/// Options for `navigator.credentials.create()`. The user handle is the
/// account id, so the authenticator never learns the email as an identifier
/// and the handle survives email changes.
pub async fn start_registration(
    pool: &Pool,
    rp: &RelyingParty,
    user: &User,
    display_name: &str,
) -> Result<Value, PasskeyError> {
    let existing = sqlx::query!("SELECT credential_id, transports FROM passkeys WHERE user_id = $1", user.id)
        .fetch_all(pool)
        .await?;
    if existing.len() as i64 >= MAX_PER_USER {
        return Err(Rejected("You already have the maximum number of passkeys. Remove one first.").into());
    }
    let challenge = issue_challenge(pool, Some(user.id), "register").await?;
    let display_name = display_name.trim();
    Ok(json!({
        "publicKey": {
            "rp": { "id": rp.id, "name": rp.name },
            "user": {
                "id": b64url_encode(user.id.as_bytes()),
                "name": user.email,
                "displayName": if display_name.is_empty() { user.email.as_str() } else { display_name },
            },
            "challenge": b64url_encode(&challenge),
            "pubKeyCredParams": ALGORITHMS.iter().map(|alg| json!({ "type": "public-key", "alg": alg })).collect::<Vec<_>>(),
            "timeout": 300_000,
            "excludeCredentials": existing.iter().map(|row| json!({
                "type": "public-key",
                "id": b64url_encode(&row.credential_id),
                "transports": row.transports,
            })).collect::<Vec<_>>(),
            "authenticatorSelection": {
                "residentKey": "required",
                "requireResidentKey": true,
                "userVerification": "required",
            },
            "attestation": "none",
            "extensions": { "credProps": true },
        }
    }))
}

/// Verify the browser's response and store the passkey for `user_id`.
pub async fn finish_registration(
    pool: &Pool,
    rp: &RelyingParty,
    user_id: Uuid,
    response: RegistrationResponse,
) -> Result<Passkey, PasskeyError> {
    if response.kind != "public-key" {
        return Err(Rejected("Unsupported credential type.").into());
    }
    let name = normalize_name(&response.name)?;
    let transports = normalize_transports(&response.response.transports);
    let credential_id = b64url_decode(&response.raw_id)?;
    let client_data_json = b64url_decode(&response.response.client_data_json)?;
    let attestation_object = b64url_decode(&response.response.attestation_object)?;
    let challenge = challenge_of(&client_data_json)?;
    if !consume_challenge(pool, &challenge, Some(user_id), "register").await? {
        return Err(Rejected("This registration request has expired. Try again.").into());
    }
    let registration =
        webauthn::verify_registration(rp, &challenge, &credential_id, &client_data_json, &attestation_object)?;
    let mut tx = pool.begin().await?;
    // Lock the account so concurrent registrations cannot exceed the limit.
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user_id).fetch_optional(&mut *tx).await?;
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM passkeys WHERE user_id = $1"#, user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_PER_USER {
        return Err(Rejected("You already have the maximum number of passkeys. Remove one first.").into());
    }
    let passkey = sqlx::query_as!(
        Passkey,
        r#"INSERT INTO passkeys (id, user_id, credential_id, public_key, algorithm, sign_count, transports, backed_up, name)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (credential_id) DO NOTHING
           RETURNING id, name, created_at, last_used_at, backed_up, transports"#,
        Uuid::now_v7(),
        user_id,
        registration.credential_id,
        registration.public_key,
        i32::try_from(registration.algorithm).map_err(|_| Rejected("Unsupported passkey algorithm."))?,
        i64::from(registration.sign_count),
        &transports,
        registration.backed_up,
        name
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Rejected("This passkey is already registered."))?;
    tx.commit().await?;
    Ok(passkey)
}

/// Options for `navigator.credentials.get()`: no account is named, so the
/// authenticator offers the user's discoverable credentials for this site.
pub async fn start_authentication(pool: &Pool, rp: &RelyingParty) -> Result<Value, PasskeyError> {
    let challenge = issue_challenge(pool, None, "sign_in").await?;
    Ok(json!({
        "publicKey": {
            "challenge": b64url_encode(&challenge),
            "rpId": rp.id,
            "timeout": 300_000,
            "userVerification": "required",
            "allowCredentials": [],
        }
    }))
}

/// Verify an assertion and return the account it proves. The caller starts
/// the session; nothing here does.
pub async fn finish_authentication(
    pool: &Pool,
    rp: &RelyingParty,
    response: AuthenticationResponse,
) -> Result<User, PasskeyError> {
    if response.kind != "public-key" {
        return Err(Rejected("Unsupported credential type.").into());
    }
    let credential_id = b64url_decode(&response.raw_id)?;
    let client_data_json = b64url_decode(&response.response.client_data_json)?;
    let authenticator_data = b64url_decode(&response.response.authenticator_data)?;
    let signature = b64url_decode(&response.response.signature)?;
    let user_handle = response.response.user_handle.as_deref().map(b64url_decode).transpose()?;
    let challenge = challenge_of(&client_data_json)?;
    if !consume_challenge(pool, &challenge, None, "sign_in").await? {
        return Err(Rejected("This sign-in request has expired. Try again.").into());
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        r#"SELECT p.id, p.public_key, p.sign_count, u.id AS user_id, u.email, u.created_at, u.is_admin, u.avatar_key
           FROM passkeys p JOIN users u ON u.id = p.user_id
           WHERE p.credential_id = $1 FOR UPDATE OF p"#,
        credential_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Rejected("This passkey is not registered here. Sign in with your password and add it in settings."))?;
    if user_handle.is_some_and(|handle| handle != row.user_id.as_bytes()) {
        return Err(Rejected("This passkey does not belong to the account it claims.").into());
    }
    let assertion = webauthn::verify_assertion(
        rp,
        &challenge,
        &row.public_key,
        u32::try_from(row.sign_count).unwrap_or(u32::MAX),
        &client_data_json,
        &authenticator_data,
        &signature,
    )?;
    sqlx::query!(
        "UPDATE passkeys SET sign_count = $2, backed_up = $3, last_used_at = now() WHERE id = $1",
        row.id,
        i64::from(assertion.sign_count),
        assertion.backed_up
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(User {
        id: row.user_id,
        email: row.email,
        created_at: row.created_at,
        is_admin: row.is_admin,
        avatar_key: row.avatar_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Algorithm, Authenticator};

    const ORIGIN: &str = "https://example.com";

    fn rp() -> RelyingParty {
        RelyingParty::from_site_url(ORIGIN, "Example").unwrap()
    }

    fn registration(authenticator: &mut Authenticator, options: &Value, name: &str) -> RegistrationResponse {
        serde_json::from_value(authenticator.create_json(ORIGIN, options, name)).unwrap()
    }

    fn assertion(authenticator: &mut Authenticator, options: &Value, user: &User) -> AuthenticationResponse {
        serde_json::from_value(authenticator.get_json(ORIGIN, options, user.id.as_bytes())).unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn register_sign_in_and_remove(pool: Pool) {
        let rp = rp();
        let user = crate::users::sign_up(&pool, "pk@example.com", "correct-horse".into()).await.unwrap();
        for algorithm in [Algorithm::Es256, Algorithm::Ed25519] {
            let mut authenticator = Authenticator::new(algorithm);
            let options = start_registration(&pool, &rp, &user, "Ann").await.unwrap();
            assert_eq!(options["publicKey"]["user"]["displayName"], "Ann");
            assert_eq!(options["publicKey"]["user"]["name"], "pk@example.com");
            assert_eq!(options["publicKey"]["authenticatorSelection"]["userVerification"], "required");
            assert_eq!(options["publicKey"]["attestation"], "none");
            let passkey =
                finish_registration(&pool, &rp, user.id, registration(&mut authenticator, &options, "  My laptop "))
                    .await
                    .unwrap();
            assert_eq!(passkey.name, "My laptop");
            assert!(passkey.backed_up && passkey.last_used_at.is_none());
            assert_eq!(passkey.transports, ["hybrid", "internal"]);

            // The challenge was single use.
            let again = registration(&mut authenticator, &options, "x");
            assert!(matches!(finish_registration(&pool, &rp, user.id, again).await, Err(PasskeyError::Rejected(_))));

            let options = start_authentication(&pool, &rp).await.unwrap();
            assert_eq!(options["publicKey"]["userVerification"], "required");
            assert_eq!(options["publicKey"]["allowCredentials"], json!([]));
            let signed_in =
                finish_authentication(&pool, &rp, assertion(&mut authenticator, &options, &user)).await.unwrap();
            assert_eq!(signed_in.id, user.id);
            let listed = list(&pool, user.id).await.unwrap();
            let stored = listed.iter().find(|p| p.id == passkey.id).unwrap();
            assert!(stored.last_used_at.is_some());

            // Replaying the same assertion fails: the challenge is gone.
            let options = start_authentication(&pool, &rp).await.unwrap();
            let replay = assertion(&mut authenticator, &options, &user);
            finish_authentication(&pool, &rp, assertion(&mut authenticator, &options, &user)).await.unwrap();
            assert!(matches!(finish_authentication(&pool, &rp, replay).await, Err(PasskeyError::Rejected(_))));
        }
        assert_eq!(list(&pool, user.id).await.unwrap().len(), 2);
        // Subsequent registrations exclude the existing credentials.
        let options = start_registration(&pool, &rp, &user, "").await.unwrap();
        assert_eq!(options["publicKey"]["excludeCredentials"].as_array().unwrap().len(), 2);
        assert_eq!(options["publicKey"]["user"]["displayName"], "pk@example.com");

        let other = crate::users::sign_up(&pool, "other@example.com", "correct-horse".into()).await.unwrap();
        let first = list(&pool, user.id).await.unwrap()[0].id;
        assert!(!remove(&pool, other.id, first).await.unwrap());
        assert!(remove(&pool, user.id, first).await.unwrap());
        assert_eq!(list(&pool, user.id).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn sign_in_rejects_unknown_wrong_user_and_cloned_passkeys(pool: Pool) {
        let rp = rp();
        let user = crate::users::sign_up(&pool, "pk@example.com", "correct-horse".into()).await.unwrap();
        let other = crate::users::sign_up(&pool, "other@example.com", "correct-horse".into()).await.unwrap();
        let mut authenticator = Authenticator::new(Algorithm::Es256);
        let options = start_registration(&pool, &rp, &user, "").await.unwrap();
        finish_registration(&pool, &rp, user.id, registration(&mut authenticator, &options, "")).await.unwrap();

        let unknown_options = start_authentication(&pool, &rp).await.unwrap();
        let mut unknown = Authenticator::new(Algorithm::Es256);
        let error =
            finish_authentication(&pool, &rp, assertion(&mut unknown, &unknown_options, &user)).await.unwrap_err();
        assert!(error.to_string().contains("not registered"), "{error}");

        let options = start_authentication(&pool, &rp).await.unwrap();
        let error =
            finish_authentication(&pool, &rp, assertion(&mut authenticator, &options, &other)).await.unwrap_err();
        assert!(error.to_string().contains("does not belong"), "{error}");

        // Legitimate use advances the counter; a stale counter is a clone signal.
        let options = start_authentication(&pool, &rp).await.unwrap();
        finish_authentication(&pool, &rp, assertion(&mut authenticator, &options, &user)).await.unwrap();
        authenticator.sign_count = 0;
        let options = start_authentication(&pool, &rp).await.unwrap();
        let error =
            finish_authentication(&pool, &rp, assertion(&mut authenticator, &options, &user)).await.unwrap_err();
        assert!(error.to_string().contains("cloned"), "{error}");

        // A registration challenge cannot be used to sign in, and vice versa.
        let register_options = start_registration(&pool, &rp, &user, "").await.unwrap();
        let mut misuse = Authenticator::new(Algorithm::Es256);
        let mut swapped = assertion(
            &mut misuse,
            &json!({"publicKey": {"challenge": register_options["publicKey"]["challenge"]}}),
            &user,
        );
        swapped.response.user_handle = None;
        assert!(finish_authentication(&pool, &rp, swapped).await.unwrap_err().to_string().contains("expired"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn registration_validates_names_limits_and_expiry(pool: Pool) {
        let rp = rp();
        let user = crate::users::sign_up(&pool, "pk@example.com", "correct-horse".into()).await.unwrap();
        let options = start_registration(&pool, &rp, &user, "").await.unwrap();
        let mut authenticator = Authenticator::new(Algorithm::Es256);
        let bad_name = registration(&mut authenticator, &options, &"x".repeat(65));
        let error = finish_registration(&pool, &rp, user.id, bad_name).await.unwrap_err();
        assert!(error.to_string().contains("64 characters"), "{error}");
        // Name validation happens before the challenge is consumed, so a retry works.
        finish_registration(&pool, &rp, user.id, registration(&mut authenticator, &options, "")).await.unwrap();

        let options = start_registration(&pool, &rp, &user, "").await.unwrap();
        sqlx::query!("UPDATE passkey_challenges SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        let mut late = Authenticator::new(Algorithm::Es256);
        let error = finish_registration(&pool, &rp, user.id, registration(&mut late, &options, "")).await.unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
        // Expired rows are swept when the next challenge is issued.
        start_authentication(&pool, &rp).await.unwrap();
        let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM passkey_challenges"#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        for _ in 1..MAX_PER_USER {
            let options = start_registration(&pool, &rp, &user, "").await.unwrap();
            let mut another = Authenticator::new(Algorithm::Ed25519);
            finish_registration(&pool, &rp, user.id, registration(&mut another, &options, "")).await.unwrap();
        }
        let error = start_registration(&pool, &rp, &user, "").await.unwrap_err();
        assert!(error.to_string().contains("maximum"), "{error}");
        assert_eq!(list(&pool, user.id).await.unwrap().len() as i64, MAX_PER_USER);

        // Deleting the account removes its passkeys and pending challenges.
        start_authentication(&pool, &rp).await.unwrap();
        sqlx::query!("DELETE FROM users WHERE id = $1", user.id).execute(&pool).await.unwrap();
        assert!(list(&pool, user.id).await.unwrap().is_empty());
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn registration_rechecks_the_limit_after_challenges_have_been_issued(pool: Pool) {
        let rp = rp();
        let user = crate::users::sign_up(&pool, "limit@example.com", "correct-horse".into()).await.unwrap();
        // Every browser tab starts while the account still has capacity.
        let mut responses = Vec::new();
        for _ in 0..=MAX_PER_USER {
            let options = start_registration(&pool, &rp, &user, "").await.unwrap();
            let mut authenticator = Authenticator::new(Algorithm::Es256);
            responses.push(registration(&mut authenticator, &options, "Laptop"));
        }
        let overflow = responses.pop().unwrap();
        for response in responses {
            finish_registration(&pool, &rp, user.id, response).await.unwrap();
        }
        let before = list(&pool, user.id).await.unwrap();
        assert_eq!(before.len() as i64, MAX_PER_USER);
        let error = finish_registration(&pool, &rp, user.id, overflow).await.unwrap_err();
        assert!(matches!(error, PasskeyError::Rejected(_)));
        assert!(error.to_string().contains("maximum"));
        let after = list(&pool, user.id).await.unwrap();
        assert_eq!(
            after.iter().map(|p| p.id).collect::<Vec<_>>(),
            before.iter().map(|p| p.id).collect::<Vec<_>>(),
            "rejected registration must not replace any credential"
        );
    }
}
