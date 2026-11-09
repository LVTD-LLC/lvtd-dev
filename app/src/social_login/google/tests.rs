use super::*;
use crate::social_login::Client as _;
use axum::{
    Json, Router,
    extract::{Form, State},
    routing::{get, post},
};
use jsonwebtoken::{EncodingKey, Header, encode, jwk::Jwk};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const CLIENT_ID: &str = "test.apps.googleusercontent.com";

pub(in crate::social_login) struct Mock {
    pub claims: Arc<Mutex<Value>>,
    pub calls: Arc<AtomicUsize>,
    pub last_form: Arc<Mutex<HashMap<String, String>>>,
    keys: Arc<Mutex<Value>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(in crate::social_login) async fn fixture() -> (Client, Mock) {
    let claims = Arc::new(Mutex::new(json!({
        "sub": "google-subject-123", "email": "google-user@gmail.com", "email_verified": true,
        "iss": "https://accounts.google.com", "aud": CLIENT_ID,
        "iat": time::OffsetDateTime::now_utc().unix_timestamp(),
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
    })));
    let key = EncodingKey::from_rsa_der(include_bytes!("test-key.der"));
    let mut jwk = Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap();
    jwk.common.key_id = Some("test-key".into());
    let keys = Arc::new(Mutex::new(json!({"keys": [jwk]})));
    let calls = Arc::new(AtomicUsize::new(0));
    let last_form = Arc::new(Mutex::new(HashMap::new()));
    let app = Router::new()
        .route(
            "/token",
            post({
                let key = key.clone();
                let last_form = last_form.clone();
                move |State(claims): State<Arc<Mutex<Value>>>, Form(form): Form<HashMap<String, String>>| {
                    let key = key.clone();
                    let last_form = last_form.clone();
                    async move {
                        let mut payload = claims.lock().await.clone();
                        if payload.get("nonce").is_none() {
                            payload["nonce"] = json!(form["code"]);
                        }
                        *last_form.lock().await = form;
                        let mut header = Header::new(Algorithm::RS256);
                        header.kid = Some("test-key".into());
                        Json(json!({"id_token": encode(&header, &payload, &key).unwrap()}))
                    }
                }
            }),
        )
        .route(
            "/keys",
            get({
                let calls = calls.clone();
                let keys = keys.clone();
                move || {
                    let keys = keys.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        ([("cache-control", "max-age=3600")], Json(keys.lock().await.clone()))
                    }
                }
            }),
        )
        .with_state(claims.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut client = Client::new(
        Config { client_id: CLIENT_ID.into(), client_secret: "test-only-secret".into() },
        "http://localhost:3000",
    )
    .unwrap();
    client.token_url = format!("{origin}/token");
    client.keys_url = format!("{origin}/keys");
    (client, Mock { claims, calls, last_form, keys, task })
}

#[test]
fn configuration_fails_closed() {
    assert!(Config::parse("".into(), "".into()).unwrap().is_none());
    for (id, secret) in [(CLIENT_ID, ""), ("", "secret"), ("bad", "secret"), (CLIENT_ID, " ")] {
        assert!(Config::parse(id.into(), secret.into()).is_err());
    }
    for url in
        ["http://example.com", "https://example.com/path", "https://user@example.com", "https://example.com?query=x"]
    {
        assert!(Client::new(Config::parse(CLIENT_ID.into(), "secret".into()).unwrap().unwrap(), url).is_err());
    }
}

#[tokio::test]
async fn validates_signed_tokens_and_caches_keys() {
    let (client, mock) = fixture().await;
    for _ in 0..2 {
        assert_eq!(client.exchange("nonce", "verifier", "nonce").await.unwrap().sub, "google-subject-123");
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let form = mock.last_form.lock().await;
    assert_eq!(form["code_verifier"], "verifier");
    assert_eq!(form["redirect_uri"], "http://localhost:3000/auth/google/callback");
    assert_eq!(form["client_secret"], "test-only-secret");
}

#[tokio::test]
async fn rejects_wrong_claims_signatures_and_algorithms() {
    let (client, mock) = fixture().await;
    let original = mock.claims.lock().await.clone();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    for (field, value) in [
        ("aud", json!("attacker.apps.googleusercontent.com")),
        ("iss", json!("https://attacker.test")),
        ("exp", json!(now - 60)),
        ("iat", json!(now + 60)),
        ("iat", json!(now - 601)),
        ("nonce", json!("wrong")),
        ("sub", json!("")),
        ("azp", json!("attacker")),
    ] {
        let mut altered = original.clone();
        altered[field] = value;
        *mock.claims.lock().await = altered;
        assert!(client.exchange("nonce", "verifier", "nonce").await.is_err(), "{field}");
    }
    let mut claims = original.clone();
    claims["nonce"] = json!("nonce");
    let token = encode(&Header::default(), &claims, &EncodingKey::from_secret(b"attacker")).unwrap();
    assert!(client.verify(&token, "nonce").await.is_err());
    assert!(client.verify("not.a.jwt", "nonce").await.is_err());
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let mut token = encode(&header, &claims, &EncodingKey::from_rsa_der(include_bytes!("test-key.der"))).unwrap();
    let start = token.rfind('.').unwrap() + 1;
    token.replace_range(start..start + 1, if &token[start..start + 1] == "A" { "B" } else { "A" });
    assert!(client.verify(&token, "nonce").await.is_err());
}

#[test]
fn only_authoritative_verified_emails_create_accounts() {
    let mut identity: Claims = serde_json::from_value(
        json!({"sub":"1", "email":"USER@gmail.com", "email_verified":true, "nonce":"n", "iat":1}),
    )
    .unwrap();
    assert_eq!(identity.signup_email().as_deref(), Some("user@gmail.com"));
    identity.email = Some("user@third-party.com".into());
    assert!(identity.signup_email().is_none());
    identity.hd = Some("workspace.com".into());
    assert!(identity.signup_email().is_some());
    identity.email_verified = false;
    assert!(identity.signup_email().is_none());
}

#[test]
fn configuration_from_environment() {
    const CHILD: &str = "APP_GOOGLE_CONFIG_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let config = Config::from_env().unwrap().unwrap();
        assert_eq!(config.client_id, CLIENT_ID);
        assert_eq!(config.client_secret, "test-only-secret");
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "social_login::google::tests::configuration_from_environment"])
        .env(CHILD, "1")
        .env("GOOGLE_CLIENT_ID", CLIENT_ID)
        .env("GOOGLE_CLIENT_SECRET", "test-only-secret")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn rejects_malformed_authoritative_emails() {
    for email in ["a@@gmail.com", "a b@gmail.com", "a\u{7}@gmail.com", "user@third-party.com"] {
        assert_eq!(authoritative_email(email, true, None), None, "{email:?}");
    }
    assert_eq!(authoritative_email("a@third-party.com", true, Some("")), None);
    assert_eq!(authoritative_email("a@gmail.com", false, None), None);
    assert_eq!(authoritative_email("a@workspace.com", true, Some("workspace.com")).as_deref(), Some("a@workspace.com"));
}

#[tokio::test]
async fn signing_keys_are_bounded_and_refreshes_are_throttled() {
    let (client, mock) = fixture().await;
    let original = mock.keys.lock().await.clone();
    for count in [0, 33, 34, 32] {
        *mock.keys.lock().await = json!({"keys": vec![original["keys"][0].clone(); count]});
        client.keys.lock().await.last_attempt = None;
        assert_eq!(client.exchange("nonce", "v", "nonce").await.is_ok(), count == 32, "{count}");
    }
    let calls = mock.calls.load(Ordering::SeqCst);
    client.keys.lock().await.expires = Instant::now() - Duration::from_secs(1);
    // An expired key is not accepted during the refresh cooldown.
    assert!(client.exchange("nonce", "v", "nonce").await.is_err());
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls);
    client.keys.lock().await.last_attempt = Some(Instant::now() - Duration::from_secs(31));
    assert!(client.exchange("nonce", "v", "nonce").await.is_ok());
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls + 1);
    assert!(client.keys.lock().await.expires > Instant::now() + Duration::from_secs(3500));
    // Unknown key IDs can refresh a still-valid cache, but never on every request.
    let mut rotated = original["keys"][0].clone();
    rotated["kid"] = json!("rotated");
    *mock.keys.lock().await = json!({"keys": [rotated]});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("rotated".into());
    let mut claims = mock.claims.lock().await.clone();
    claims["nonce"] = json!("nonce");
    let token = encode(&header, &claims, &EncodingKey::from_rsa_der(include_bytes!("test-key.der"))).unwrap();
    assert!(client.verify(&token, "nonce").await.is_err());
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls + 1);
    client.keys.lock().await.last_attempt = Some(Instant::now() - Duration::from_secs(31));
    assert!(client.verify(&token, "nonce").await.is_ok());
    assert_eq!(mock.calls.load(Ordering::SeqCst), calls + 2);
}

#[tokio::test]
async fn subject_and_token_size_limits_accept_the_boundary() {
    let (client, mock) = fixture().await;
    for size in [255, 256, 257] {
        mock.claims.lock().await["sub"] = json!("s".repeat(size));
        assert_eq!(client.exchange("nonce", "v", "nonce").await.is_ok(), size == 255);
    }
    let mut claims = mock.claims.lock().await.clone();
    claims["sub"] = json!("valid-subject");
    claims["nonce"] = json!("nonce");
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    let key = EncodingKey::from_rsa_der(include_bytes!("test-key.der"));
    // JWT base64 length advances by one or two; find a valid exact-boundary token.
    let mut boundary = None;
    for token_type in ["JWT", "JWT-test"] {
        header.typ = Some(token_type.into());
        claims["padding"] = json!("");
        let base_len = encode(&header, &claims, &key).unwrap().len();
        let estimate = (16_384 - base_len) * 3 / 4;
        for size in estimate.saturating_sub(3)..=estimate + 3 {
            claims["padding"] = json!("x".repeat(size));
            let token = encode(&header, &claims, &key).unwrap();
            if token.len() == 16_384 {
                boundary = Some(token);
                break;
            }
        }
        if boundary.is_some() {
            break;
        }
    }
    assert!(client.verify(&boundary.expect("exact boundary token"), "nonce").await.is_ok());
    claims["padding"] = json!("x".repeat(13_000));
    let oversized = encode(&header, &claims, &key).unwrap();
    assert!(oversized.len() > 16_384);
    assert!(client.verify(&oversized, "nonce").await.is_err());
}

#[tokio::test]
async fn provider_json_rejects_errors_and_oversized_bodies() {
    let app = Router::new()
        .route(
            "/{size}",
            get(|axum::extract::Path(size): axum::extract::Path<usize>| async move {
                format!("\"{}\"", "x".repeat(size - 2))
            }),
        )
        .route("/error", get(|| async { (axum::http::StatusCode::BAD_GATEWAY, "\"valid JSON\"") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = reqwest::Client::new();
    for size in [65_536, 65_537, 70_000] {
        let response = http.get(format!("{origin}/{size}")).send().await.unwrap();
        let parsed = bounded_json::<String>(response).await;
        assert_eq!(parsed.is_ok(), size == 65_536, "{size}");
    }
    let response = http.get(format!("{origin}/error")).send().await.unwrap();
    assert!(bounded_json::<String>(response).await.is_err());
    task.abort();
}

#[tokio::test]
async fn issued_at_accepts_only_the_documented_clock_window() {
    let (client, mock) = fixture().await;
    for (offset, accepted) in [(-600, true), (-601, false), (30, true), (31, false)] {
        let mut checked = false;
        for _ in 0..10 {
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            mock.claims.lock().await["iat"] = json!(now + offset);
            let result = client.exchange("nonce", "v", "nonce").await;
            // Retry if the wall clock crossed a second while signing/verifying.
            if now == time::OffsetDateTime::now_utc().unix_timestamp() {
                assert_eq!(result.is_ok(), accepted, "iat offset {offset}");
                checked = true;
                break;
            }
        }
        assert!(checked, "could not check the clock boundary within one second");
    }
}

#[test]
fn authorization_request_binds_state_nonce_and_pkce_without_exposing_secret() {
    let client = Client::new(
        Config { client_id: CLIENT_ID.into(), client_secret: "test-only-secret".into() },
        "https://app.example.com",
    )
    .unwrap();
    let url = reqwest::Url::parse(&client.authorization_url(
        "browser-state",
        "token-nonce",
        "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    ))
    .unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    assert_eq!(url.path(), "/o/oauth2/v2/auth");
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["redirect_uri"], "https://app.example.com/auth/google/callback");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["scope"], "openid email");
    assert_eq!(query["state"], "browser-state");
    assert_eq!(query["nonce"], "token-nonce");
    assert_eq!(query["code_challenge"], "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["prompt"], "select_account");
    assert_eq!(query.len(), 9);
    assert!(!url.as_str().contains("test-only-secret"));
}
